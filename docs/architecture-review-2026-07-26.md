# Architecture & code review — 2026-07-26

Method: 12 parallel subsystem readers produced an architecture map; 18 cross-cutting auditors
worked from it; every finding was then attacked by two independent adversarial lenses (*is this
factually true of the code right now?* and *is it worth doing for this project?*). 179 raw findings
→ **53 survived both lenses**, 84 were refuted or materially corrected, 42 never got a verifier.
Every claim below carries a `file:line` I or a verifier re-opened. Coverage caveats in §7.

---

## 1. Verdict

This is a genuinely well-built codebase, and the review should be read against that. The parts
that were *designed* — `plex/` as a typed client, `ui/` as a token+component design system,
`player/shared.rs` as a declared single point of cross-thread state, `posters.rs`/`browse.rs`/
`metadata.rs` as generation-guarded mailbox workers — are all cleaner than the platform deserves.
The device knowledge encoded in the four `CLAUDE.md` files is the project's most valuable asset,
and the discipline around it is real: `browse.rs:547` literally says *"captured on the main thread;
the worker must not read the statics."*

The structural weakness is not the 172 `static mut` — the sweeping version of that finding was
refuted, and correctly: the overwhelming majority are main-thread-confined or write-once GL
locations, and converting them buys nothing. **The weakness is that the codebase has no way to say
"that failed."** There is no error type, no fault channel from the player to the UI, no HTTP status
above the socket, and no load-state on any screen. 101 functions return `Option`; 4 return `Result`.
A 401, a 500, a reset mid-body and an unplugged cable are one value — `None` — and the layers above
turn that single value into a permanently blank Home, a detail page showing the *previous* movie
under the *new* movie's backdrop, and a black player screen with no timeout and no message. That
one gap generates more confirmed high-severity findings than every other theme combined.

Second: `route.rs` never finished its migration. It models one object — "the current playback" — as
16 loose `static mut` with 14 getters, 5 setters, no owner and no reset. That has *already* shipped
a live ordering bug (§2.2), and it is the only module in the crate whose globals are read from
worker threads, which is two real data races on armv7.

Third, and cheapest to fix: the build can silently ship a wrong binary — `Makefile` is a
prerequisite of zero targets, and the 22 assets the crate embeds via `include_str!` are in no
dependency list. On a project whose entire verification loop is "observe behaviour on the device,"
a build that lies is the most expensive possible failure.

---

## 2. Structural findings

### 2.1 There is no failure channel — from any layer to any layer

`player/shared.rs` is documented as the *only* cross-thread state, and it has no error field
(`grep 'fault\|error\|Err' player/shared.rs` → zero hits). So **eight distinct fatal demux errors
exit through the same door as a finished movie**: `http_open` failure (`ff.rs:1272`), `av_malloc`
(`:1290`), `avio_alloc_context` (`:1303`), `avformat_alloc_context` (`:1309`), `open_input`
(`:1316`), `find_stream_info` (`:1321`), no video stream (`:1339`), `packet_alloc` (`:1429`) — all
`break` to `aq_set_eof(); aq_set_eof(); log("ff: demux ended")` at `ff.rs:1597`. A 404 on the part
URL, a token revoked between `/decision` and the GET, or a PMS restart therefore leaves the app on
a black Player screen forever, with the video plane bound and nothing decoded, recoverable only by
BACK.

The same shape repeats upward. `plex::Client::get_json` (`client.rs:98-102`) collapses transport
failure and parse failure into `None`, and **the entire `plex/` + `stream.rs` layer emits zero log
lines** — the app's documented primary debugging surface records nothing for the single most
common class of user-visible failure. `pms_fetch_hubs` then *commits an empty catalog* on failure
(`pms.rs:226-229`, `:328-333`), so one failed request after playback blanks Home. And
`metadata::load_detail` returns without touching `CURRENT` on failure (`metadata.rs:478-481`) while
`detail::open_rk` has already committed the new identity (`ui/detail.rs:1187-1193`) — so a Wi-Fi
blip shows one movie's synopsis, cast and episode list under another movie's backdrop, and Play
starts something other than what was read.

**Fix.** Three small, independent pieces, none of which needs a crate-wide error type:
`SHARED.fault: Mutex<Option<Fault>>` set at each `ff.rs` break and surfaced by `pump`; make
`http_get`/`http_post` return the status they already parse at `stream.rs:193` and add ~5 `log()`
calls at the `client.rs` choke points; a `Load<T> { Idle, Loading, Ready(T), Failed }` in the
content layer with one shared placeholder view. Adopt in `detail` first (it has the wrong-item
bug), `home` second.

### 2.2 `route.rs` models one object as 16 loose globals — and it already cost a correctness bug

`route.rs:10-46` declares `URL`, `TSESSION`, `CUR_REMUX`, `CUR_RK`, `CUR_AUDIO_SID`, `CUR_SUB_SID`,
`CUR_PART_ID`, `SESS`, `MACHINE_ID`, `PQ_ID`, `PQ_ITEM_ID`, `STREAM_VCODEC`, `STREAM_ACODEC`,
`STREAM_FPS`, `TITLE: [c_char;128]`, `CTXLINE: [c_char;96]` — all fields of one "current playback",
written inline through `addr_of_mut!` in three separate `unsafe` blocks with blocking network I/O
in between.

**The live bug** (verified directly): `build_stream` calls `put_selection()` at `route.rs:424`;
`put_selection` reads `CUR_PART_ID` and returns early if it is `≤ 0` (`route.rs:258-259`); but
`CUR_PART_ID` is only written *after* `build_stream` returns — `route.rs:517` (`play_movie`) and
`route.rs:542` (`play_episode`). Those are the only two writes in the crate. So the stream-selection
PUT is skipped entirely on the first play of the process, and thereafter **always targets the
previous item's part**. Every non-MKV item takes the remux branch (`route.rs:369-370`), so this is
the normal path for mp4/mov: a server-default subtitle is never suppressed and gets burned into a
transcode nobody asked for.

**Fix.** Collapse to `static mut SESSION: Option<PlaybackSession>` with an accessor, matching
`engine.rs:101` and `metadata.rs:133`. Build the whole struct in locals inside `build_stream`
(part id included) and commit it in one assignment — `pms.rs:226-228` already documents exactly
this build-locally-commit-atomically rationale. That deletes the ordering bug by construction,
gives `reset()` for free, and makes a new per-item field impossible to forget.

### 2.3 Two real data races, both from `route.rs` into worker threads

`player/CLAUDE.md:34` states the rule — *"`shared.rs` — the only cross-thread state … don't smuggle
it through a raw static."* `route.rs` is the sole violation, and it is reached from two workers:

- **Demux thread** → `ff.rs:1336` calls `route::stream_acodec()` = `(*addr_of!(STREAM_ACODEC)).clone()`
  (`route.rs:97-99`), inside `demux`'s `'outer: loop` so on *every* reopen. The main thread writes
  the same global via `route::set_stream_acodec()` on an audio-track switch. Cloning a `String`
  while another thread reassigns it is a heap use-after-free.
- **Timeline thread** → `player/threads.rs:51` calls `route::report_timeline`, which reads
  `cur_audio_sid()` / `cur_sub_sid()` (`route.rs:651-652`) — mutated on the main thread by
  `commit_audio_selection`. The function's own doc comment says `rk` is captured at spawn *"no
  static-mut race"*, so the hazard class was known; the two `i64` reads two lines below are exactly it.

Verifiers narrowed the scope (`SESS`/`PQ_ID`/`PQ_ITEM_ID` are only written on the entry path, so
those three are not races) and argued *high* rather than *critical* because the write windows are
narrow. The mechanism is not in dispute.

**Fix.** Move the payload, not the pointer: build `plex::TimelineReport` on the main thread and move
it into `timeline_thread` at spawn — `engine.rs:310-316` already does this for `rk`. Promote the two
mutable ids to `AtomicI64` in `SHARED`. For the demuxer, publish the wanted codec as
`SHARED.wanted_acodec: Mutex<String>` (`shared.rs` already holds five `Mutex`es and is the
sanctioned home).

### 2.4 The transport is the weakest layer in the app

Four confirmed defects in `stream.rs`, all on the path every screen depends on:

| | Site | Problem |
|---|---|---|
| No connect timeout | `stream.rs:122-126` | `connect()` is blocking with no deadline; the only timeout, `SO_RCVTIMEO`, is set at `:131` — *after* `connect` returns, so it cannot bound it. An unreachable PMS freezes the SDL main loop for the kernel's SYN-retry budget (~2 min), multiplied by every request in the chain. |
| Status discarded | `stream.rs:208-212` | The status is parsed at `:193` then thrown away; `http_get` returns `None`. 401, 404, 500, 3xx and "cable unplugged" are one value. There is no re-auth path anywhere (`grep -i 401 rust-modules/src` → nothing in the PMS layer). |
| `close()` as interrupt | `stream.rs:290-300` | `pump.rs:75`, `pump.rs:146` and `engine.rs:482` close the socket to wake a demux thread blocked in `recv`. On Linux `close(2)` does not wake a blocked reader — `shutdown(2)` does. So BACK during a stall blocks the main loop on `join()` for up to the 15 s `SO_RCVTIMEO`. `fd` is a bare `c_int` (`stream.rs:10`) mutated from two threads, so the freed number can also be recycled by a poster worker. |
| No completeness check | `stream.rs:316-324` | The struct tracks `content_length` and `consumed` faithfully, then the one-shot wrappers ignore both and return `Some(body)` unconditionally. A body truncated by a reset is a "successful" response. |

The nonblocking-connect fix is ~15 lines of `libc` and no new dependency. `fd` → `AtomicI32` plus
`shutdown`-to-interrupt / owner-only-`close` also lets the 1200 ms `SEEK_STUCK_MS` watchdog
(`pump.rs:60`) stop being load-bearing — it exists because the wrong syscall was used.

### 2.5 `app.rs` is the de-facto application object

2080 lines holding SDL bring-up, input decode, lifecycle, draw orchestration, pump orchestration,
**the player's entire transport state machine**, and **~40 dev triggers**.

- The scrub/HUD-focus machine lives as ten loop locals (`app.rs:475-503`) spread over ~250 lines
  (keyup commit `:846`, hold engage `:872`, focus walk `:1052`, OK dispatch `:1122`, scrub seed
  `:1225`, pointer drag `:1323`, accel advance `:1791`, tap debounce `:1817`). This is the most
  intricate interaction in the app — a Magic Remote that emits held keys as `0x101` auto-repeats
  with one keyup, needing a lost-keyup net and a tap-coalescing debounce — and it is un-reviewable
  in isolation. It also makes `player_hud` the only screen with no focus model of its own.
- Trigger reads are scattered across ~70 sites, with `app.rs:1497-1719` being one **223-line
  contiguous block** of in-loop firing, plus nine bespoke `*_tried` latch bools and a
  hand-maintained `DIAG` allow-list (`app.rs:339`) that **has already drifted** (§2.7).
- Pointer dispatch is hand-copied per route (`app.rs:1335-1355` motion, `:1356-1458` click) and
  **there is no `Route::Detail` arm in either chain** — so with the Magic Remote pointer you cannot
  press Play, pick an episode, or open a Related item. It also breaks the project's own headless
  driver: `tools/stream-screen.py`'s `ck:X,Y` clicks are inert on the one screen that starts playback.

Note: the sweeping *"introduce a `Screen` trait, collapse all eight dispatch chains"* proposal was
**refuted** by two independent verifiers — `docs/ui-viewtree-plan.md` records deliberate carve-outs
(focused-last z-order, `player_hud`'s `SCR_H`-offset geometry shared with `app.rs` hit-tests) that a
uniform trait would break. Do the three narrow extractions instead: `HudState`+`ScrubMachine` into
`ui/player_hud.rs`, a `triggers.rs` with one table (so `DIAG` cannot drift from the catalog), and a
`Route::Detail` arm in the two pointer chains.

### 2.6 The build can silently ship a wrong binary

All three verified empirically against this checkout:

- **`Makefile` is a prerequisite of zero targets** (`grep` over every rule: 0 hits). Change
  `RUSTFLAGS_TV`, `CFLAGS`, or a stub SONAME and `make` rebuilds nothing.
- **`$(RUST_LIB)`'s dependency list omits every embedded asset** (`Makefile:88`): the 9
  `rust-modules/src/shaders/*.{vert,frag}` pulled in by `include_str!` at `gfx.rs:35` and the 13
  `assets/icons/*.svg` at `ui/icons.rs:35-47`, plus `Cargo.lock`. Edit a shader or an icon, `make
  deploy`, and the TV runs the old one — a UI change that looks like it had no effect.
- **The SIGILL-preventing codegen flags exist only as an env var inside one recipe** (`Makefile:87`).
  There is no `.cargo/config.toml`, so a plain `cargo build --target arm-unknown-linux-gnueabi`
  from an IDE or an agent writes an ARMv6/CP15 staticlib to the *same path* — cargo fingerprints
  `RUSTFLAGS`, so it replaces the good artifact — and `make` links it without comment. A verifier
  additionally found the existing detector is broken: `tools/crash-report.sh:72` greps
  `'mcr.*15.*c7, c10'` but the NDK objdump prints `cr7, cr10`.
- **`make ipk` ships no fonts** (`Makefile:163-165` copies 4 files; `deploy` copies 6). Confirmed
  against `ipkroot/data.tar.gz`. The ipk is the only artifact a non-developer receives, and on a
  clean install the app renders the entire `theme::size` ladder in DroidSans — invalidating the
  whole rasterization contract that `tools/font-hint-audit.py` exists to guard.
- **No lint gate of any kind**: no `.github/`, `rust-toolchain.toml`, `clippy.toml`, `rustfmt.toml`,
  `.cargo/config.toml`, no `#![deny(warnings)]`, no checker target. Meanwhile `#![allow(dead_code)]`
  sits on 14 files — switching off one of the only automatic checks available on a project with no
  host test suite, exactly where the API surface grows fastest.

### 2.7 Documentation drift in the files that carry the device knowledge

These matter more here than in a normal repo, because `CLAUDE.md` *is* the onboarding path and it
explicitly claims to be "all verified in code."

| Claim | Location | Reality |
|---|---|---|
| `stream.rs` has "**no chunked decoding**" | `CLAUDE.md:121` | Fully implemented and it is the live-transcode path: `chunked`/`chunk_left` fields (`stream.rs:17-18`), `hs_next_chunk` (`:55-86`), header sniff (`:202-203`), read branch (`:224-257`). A negative capability claim is the most dangerous kind — someone debugging a stalled transcode will rule out the framing path on the doc's authority. |
| "The focus ring/glow is shader-baked … callers drive it through a `focus: f32` scalar" | `ui/CLAUDE.md:65` | Completely dead. All **34** `Painter::rect` call sites pass `0.0`; the only other `gfx::draw_rect` caller (`gfx.rs:499`) passes `0.0`. So `glUniform1f(LOC_FOCUS, …)` (`gfx.rs:325`) uploads a provably-zero uniform per rect and `shaders/fs_src.frag:50-55` compiles an unreachable branch. The real knob (the folded shadow ramp in `tex_carded`) is undocumented. |
| `Painter::clip` has "**its one user**" and must not be used in the scroll flow | `ui/CLAUDE.md:84` | Two users. `card_row::resume_bar` (`card_row.rs:288-302`) sets and clears scissor per Continue-Watching tile, every frame, inside the shelves. `clip_clear()` is a hard reset, not a pop — an unguarded nesting hazard the doc doesn't warn about. |
| "except the logs" in the picker-suppression rule | `CLAUDE.md:229` | The code exempts three *named* logs, not "the logs" (`app.rs:339`, `DIAG: [&str; 7]`). `ui/anim.rs:107` writes a fourth — `/tmp/plxnative-anim.log` — which is not on the list, so arming the anim overlay once marks **every subsequent boot as automated forever** and the who's-watching picker never appears again. |

---

## 3. Code-level findings

Severities reflect the adversarial panel's corrections, not the original auditor's.

### Crashes / correctness

| Sev | Site | Finding |
|---|---|---|
| high | `ui/home.rs:445`, `:577` | `Grid` holds `shelves: [CardRow; MAX_HUBS]` (16, `home.rs:412`) but `draw` iterates `0..hub_count()` and `hit_at` iterates `0..hub_count()`, both indexing `self.shelves[r]`. `hub_count()` is `hubs().len()` (`pms.rs:188`) with **no cap** — `home_hubs(12)` caps items *per hub*, not hub count. 17+ non-empty hubs (ordinary with 3-4 libraries) → index-out-of-bounds panic every frame. `layout` (`:437`) and `env` (`:638`) clamp correctly, so the cap exists in two of four places. |
| high | `browse.rs:171` | `reset()` drops all three result mailboxes but never clears `FETCHING`/`GENRE_FETCHING`/`LETTERS_FETCHING` (`:125-127`), which are cleared *only* inside a successful mailbox take (`:466`, `land_directory`). Deterministic wedge: scroll Library → BACK to Home (pump stops) → worker lands → switch profile → `app.rs:320 browse::reset()` nulls the mailbox → the flag is now permanently true. Library is a spinner forever, with no log line (`browse.rs` emits none). |
| high | `route.rs:376` | The direct-play gate is defensive about video (`:364-368`, explicitly) but credulous about audio: when `pick_dp_audio` returns `None` — meaning *no* track is direct-playable — it still defers to `server_decision`, then `route.rs:383` falls back to `acodec` and publishes it. `engine.rs`'s `_ => "AC3"` fallback then mislabels it. A TrueHD-only item the server calls `directplay` gets an AC3 Load payload against a TrueHD lane: audio ES never configures, playback wedges at BufferFull. Make the audio gate symmetric with the video gate. |
| high | `plex/client.rs:146` | `install` is a `OnceLock`, so host/port freeze for the process life and the `Some(c) => c.set_token(token)` branch silently discards a new address. `sign_out` (`auth.rs:227-232`) clears everything *except* the client. When the PMS box takes a new DHCP lease, re-login installs the new token against the old IP with no recovery short of a kill. |
| medium | `ff.rs:1149`, `:1176` | `while i + nls <= size { … nl = …; if nl == 0 \|\| i + nl > size { break } }` — `usize` is **32-bit** here and `[profile.release]` sets no `overflow-checks`, so `i + nl` wraps and defeats the only bounds guard, panicking the demux thread before it can `aq_set_eof` → permanent hang, not a clean stop. `nls == 4` is the value `parse_extradata` *defaults to* on any parse failure (`ff.rs:1014/1021/1055`). Use `checked_add`, and wrap `ff::demux` in `catch_unwind` with the EOFs on the unwind path (the crate already does this at `mod.rs:281`, `browse.rs:354`, `metadata.rs:552`). |
| medium | `ff.rs:944` | `read_cb` maps *both* `http_read` return codes to `AVERROR_EOF`, discarding the distinction `stream.rs:281` vs `:283` carefully preserves. `seek_cb` never verifies the Range reopen returned `206` — a Range-ignoring proxy yields total demux corruption with libavformat parsing from byte 0 while believing it is at `target`. |
| high | `src/starfish.c:74` | `static int g_smp_ready` publishes a 64 KB in-place-constructed C++ object across threads with no `_Atomic`, no barrier, not even `volatile`. Written on the load thread (`:87`, right after `SMP_ctor`), read on the main thread every frame (`pump.rs:16`). `sf_feed` (`:148`) has **no guard at all** and relies entirely on the caller. Two lines: `_Atomic int` + release/acquire. |
| medium | `ui/home.rs:587` | The card hit-test omits the `env.sp` factor that both draw passes apply (`:465`, `:485`), so during the ~150 ms hero→grid snap the drawn and hit rects disagree by `scroll_x * (1 - sp)` — a click activates the wrong card, and `ui::press` makes the wrong card visibly dip. Six copies of one layout formula across two screens; extract `cell_rect`. |
| low | `gfx.rs:218`, `:246`, `:276`, `:289`, `text.rs:159`, `:174` | Shader compile/link failure is the only error in the crate that calls `std::process::exit(1)` after an `eprintln!` — so the one failure that kills the app at boot writes **nothing** to the event log. Every other failure in the same files routes to `crate::log` and degrades. |

### Resource lifecycle

| Sev | Site | Finding |
|---|---|---|
| medium | `text.rs:89` + `:239` | The glyph-cache key is `s: [u8; 96]`, written through `cbuf::set_bytes_raw` which truncates to **95** bytes (`cbuf.rs:16`), but the hit test compares `entry_key(e) == s_bytes` against the **full** slice. Any string ≥96 bytes therefore never matches the entry it just wrote — a permanent miss: full TTF render + ink scan + `glTexImage2D` + `glDeleteTextures` every frame, forever. Measured on the real font, ~10% of English wrapped hero lines exceed 95 bytes, and non-Latin text hits it far sooner (UTF-8 bytes, not chars). Make the key owned, or store the true length and compare it. |
| medium | `text.rs:293` | `text_width` — the *only* measurement API — measures by rasterizing: `TTF_RenderUTF8_Blended` + ink scan + `glGenTextures` + LRU eviction, to return an integer. `TextView`'s word-wrapper calls it once per word trial (`text_view.rs:128`), so a wrap-cache miss is ~60 FreeType renders + ~60 texture allocations that evict ~37% of the glyph cache. `TTF_SizeUTF8` does exactly this with no surface; add it to the `extern "C"` block. (Both layers above are already memoized, so this is a first-paint spike, not steady state.) |
| medium | `browse.rs:481` | `st.items.resize_with(st.total as usize, || None)` allocates a dense spine sized to the server's `totalSize` in one main-thread shot, and it is never evicted or shrunk for the session. `WANT` (`:103`) and the `PAGE`-sized reasoning in `maybe_spawn` (`:498`) already carry the residency hint needed to make it windowed. |
| medium | `ui/login.rs:38` | `s.qr_tex = 0` on retry without `gfx::delete_tex` — the one place in the crate where the upload/delete pair is broken (`posters.rs:83` and `player_hud.rs:116` do it right). `auth::sign_out` hits the same path. Unbounded in retries, and the sign-in screen is exactly where a flaky network leaves you. |
| medium | `player/engine.rs:103` | `engine()` hands out `&'static mut Engine`; `pump` holds that borrow for 240 lines and five arms call `teardown` → `(*addr_of_mut!(ENGINE)).take()`, moving the pointee out from under the live borrow. Benign today only because every site immediately reconstructs into the same slot. A scoped `with_engine(|e| …)` returning a `PumpAction` enum makes the hazard unrepresentable at zero runtime cost. |
| low | `ff.rs:1428` | Eight hand-maintained cleanup lists in `demux`; the newest resource (the per-track subtitle `AVCodecContext`s) is already missing from one. `Drop for Venc` (`ff.rs:697`) is the RAII pattern to copy. |

### Performance

| Sev | Site | Finding |
|---|---|---|
| low | `text.rs:414`, `gfx.rs:596` | Every text and textured draw brackets itself with `glUseProgram` + re-uploads provably-invariant uniforms (`u_screen`, `u_tex`, `glActiveTexture`), then restores the base program. ~200-260 program binds and ~300 dead uniform uploads per detail frame; ~0.1-0.4 ms on Midgard. A `use_prog()` memo in `gfx.rs` plus hoisting the constants to `init_*` is free, and it makes the implicit "PROG is bound" contract explicit. |
| low | `gfx.rs:325` | The dead `focus` uniform (§2.7) is uploaded per rect, and the `exp()`+smoothstep branch in `fs_src.frag:50-55` inflates the shader's register allocation — computed from the worst path, so it caps occupancy for *all* fragments including the radius-0 full-screen scrims. |

### UI-system drift

| Sev | Site | Finding |
|---|---|---|
| medium | `ui/mod.rs:174`, `gfx.rs:313` | The `focus: f32` parameter is dead at all 34 call sites but still threaded through `Painter::rect` → `gfx::draw_rect` → a shader branch, and still described as live by `ui/CLAUDE.md:65` and nine doc comments. Deleting it is compile-enforced. |
| medium | `ui/chapters_panel.rs:58` | Forks the episode strip with a slot-pinning scroll rule the shared component **explicitly refuses** (`card_row.rs:112-115`: *"no slot pinning, so entering a row doesn't jump it"*) and cites a parity with the episode picker that no longer exists (`detail.rs:461-468` uses `card_row::scroll_into_view`). Two different feels for the same LEFT/RIGHT gesture in one session. |
| low | `ui/widgets.rs:337` | `TabPill::width(chars, sz)` estimates from a codepoint count × a Latin advance ratio. There are **three** tab rows and they disagree: `draw_tab_row` (`widgets.rs:424`) and the detail season tabs (`detail.rs:419`) both measure with `text_width`, while the **player HUD's row uses the estimator** (`player_hud.rs:327`) — corrected by §8.c-C14; the first pass wrongly implied the estimator was vestigial. So the component that exists to unify the tab rows lays them out by two different rules, and the row still on the estimator is the one most likely to carry a non-Latin label (`chars().count()` counts codepoints, not advances). |
| low-med | `ui/mod.rs:131` | The retained-mode `View`/`Env` core is thinner than `mod.rs:1-7` advertises: `layout` and `update` have one implementor each (both `home.rs`), six of ten `impl View` ignore `Env`, and the components with a real lifecycle (`TableView`, `CardRow`, `Popover`) deliberately don't implement it. Shrink the documented contract to what is true rather than "finishing" the tree — the verifiers showed a recursive traversal would break two named invariants in `docs/ui-viewtree-plan.md`. |

### Data layer

| Sev | Site | Finding |
|---|---|---|
| high | `plex/models.rs:178` | `Media` carries only `video_codec`/`audio_codec`/`part`; `container`, `width`, `height`, `bitrate`, `videoResolution`, `audioChannels` are all absent even though `docs/pms-api.md:290-302` lists them as required for the direct-play decision, and `:219-221` warns **in bold** that the picker must iterate `Media[]` rather than take `[0]`. Every consumer takes `[0]` — `pms.rs:134`, `metadata.rs:207/229/298/346/423/434`, `route.rs`. So on a 4K-HDR + 1080p item the client always drives the 4K copy, and an `.mp4`-first item gets remuxed while a directly-playable `.mkv` sibling sits unused. |
| medium | `plex/client.rs:110` | `get_void`/`post_void` discard everything, and the callers log success unconditionally — so `timeline`, `scrobble`, `unscrobble` and `transcode_stop` print a success line whether or not they worked. **`tests/run.py`'s `timeline_climb` case greps exactly those lines**, so the harness assertion is vacuous: a real timeline regression cannot fail it. |
| medium | `stream.rs:316` | (§2.4) A truncated body is returned as a successful response — half a `/hubs` JSON becomes a serde error becomes `None` becomes a blanked Home. |

---

## 4. The plan

Four waves. Each is independently shippable and leaves `make` green. Verification is stated per
item because there is no host runtime today — except where the item *creates* one.

### Wave 0 — the build tells the truth (half a day, near-zero risk)

Do this first: every later wave's on-device verification is worthless until the build is honest.

1. Add `Makefile` to the prerequisites of `src/%.o`, `$(RUST_LIB)`, `$(STUBS)`, `pkg/plxnative`.
   Replace the hand-listed source glob at `Makefile:88` with `$(shell find rust-modules/src assets -type f)` + `Cargo.lock`.
2. Create `rust-modules/.cargo/config.toml` with the target `rustflags` and `build-std`; drop them
   from the recipe. Fix the `cr7, cr10` grep in `tools/crash-report.sh:72`.
3. Hoist an `APP_FILES` list used by both `deploy` and `ipk` so the ipk stops shipping without fonts.
4. Add a `make lint` target (`cargo clippy … -- -D warnings` + `cargo fmt --check`). Land it in
   three steps: auto-fix, then the 3 `static_mut_refs`, then enforce. Delete the 14
   `#![allow(dead_code)]` and triage the ~45 items they hide.

*Verify:* touch a shader → `make` rebuilds; `tar tzvf ipkroot/data.tar.gz` lists both fonts;
`objdump` the staticlib for CP15 barriers after a bare `cargo build`.

### Wave 1 — failures become visible (2-3 days)

5. `SHARED.fault: Mutex<Option<Fault>>`, set at all eight `ff.rs` breaks, cleared by
   `reset_session()`, surfaced by `pump`. Add the missing load watchdog: no `Stage::Streaming`
   within ~8 s of `start_bufferfeed` → synthesize a fault. *Verify:* point `/tmp/plxnative-url` at
   a 404 and assert the app leaves the player instead of sitting black.
6. `http_get`/`http_post`/`http_put` return the status they already parse; add ~5 `log()` calls at
   the `client.rs` choke points (path prefix only — never the query, `with_token` bakes the token
   in). Add the `consumed` vs `content_length` completeness check. *Verify:* event log shows
   `GET /hubs -> 401` against a revoked token.
7. `Load<T>` in the content layer + one shared placeholder view. Adopt in `detail` (fixes the
   wrong-item render), then `home` (stop committing an empty catalog on failure — keep the previous
   one and arm `browse.rs:471`'s `RETRY_CD` backoff). *Verify:* pull the LAN cable mid-open.
8. Clear the three single-flight flags in `browse::reset()` — or better, fold flag+mailbox into a
   `SingleFlight<T>` so the fourth mailbox someone adds is correct by construction.
9. Nonblocking `connect` + `poll(POLLOUT)` with a ~2 s budget, plus `SO_SNDTIMEO`.
   *Verify:* `make run` with the PMS host firewalled; the UI must stay responsive.

### Wave 2 — the races and the sharp edges (2-3 days)

10. `HttpStream.fd` → `AtomicI32`; `shutdown(SHUT_RDWR)` at the three interrupt sites; `close` only
    on the owning thread via `swap(-1)`. Then reconsider `SEEK_STUCK_MS`. *Verify:* the two
    rapid-seek-burst cases in `tests/run.py`, plus BACK during a stall (should be instant, not 15 s).
11. Move the `TimelineReport` payload to spawn-time; `audio/subtitle_stream_id` → `AtomicI64` in
    `SHARED`; `wanted_acodec` → `SHARED.wanted_acodec`. Then `route::` has zero worker-thread readers.
12. `_Atomic int g_smp_ready` with release/acquire, and a guard on `sf_feed`.
13. `checked_add` in both `packet_to_annexb` passes; `catch_unwind` around `ff::demux` with the
    `aq_set_eof` pair on the unwind path.
14. Clamp the home shelf count in one accessor (`n_hubs()`), used at all four sites.
15. `AVERROR(EIO)` vs `AVERROR_EOF` in `read_cb`; require `206` in `seek_cb`.

### Wave 3 — the structural work (1-2 weeks, sequenced)

16. **`PlaybackSession`** — collapse `route.rs`'s 16 globals into one `Option<PlaybackSession>`
    built in locals and committed once. This deletes the `put_selection` ordering bug by
    construction. *Verify:* the full `tests/run.py` suite; specifically assert a PUT carrying the
    *current* part id on a fresh mp4 play.
17. **Host tests.** The "no host runtime" premise is about running the *app*; it does not apply to
    the logic. **Corrected by §8.a-A1 — the original claim here was wrong.** `cargo +nightly check`
    on macOS produces exactly **one** error: `libc::__errno_location()` (`capture.rs:219`).
    `MSG_NOSIGNAL` (`capture.rs:396`) resolves fine and is *not* a blocker. The second blocker is
    the *link* step, not compilation: the four `#[link]` attrs at `ff.rs:175/220/255/270`
    (`avformat`/`avcodec`/`avutil`/`swscale`). Both are `#[cfg]`-gateable, so the real cost is ~5
    edits — but host coverage is the **pure half only**: anything touching GL fails at link
    (`gfx::frame_clear` → `_glClear`). That unlocks unit tests for: `route`'s URL
    building and direct-play decision, `plex/` model deserialization against captured fixtures,
    `aq.rs`, `cbuf.rs`, text wrap/elide, `detail.rs`'s `child_top` stacking, the seek-script parser,
    the A-Z rail prefix sums, `fmt.rs`. Given the git history — the `hero_pill_index` sentinel
    regression, the season-mailbox monotone bug, the `put_selection` ordering bug — this is the
    highest-leverage item in the whole review.
18. `Media[]` version picker: add the spec's decision fields and replace `first_part()` with
    `pick_version() -> (mediaIndex, partIndex, &MediaPart)` threaded through `TranscodeSpec`.
19. Extract `HudState`+`ScrubMachine` into `ui/player_hud.rs`; extract `triggers.rs` with one table
    so `DIAG` derives from the catalog; add the `Route::Detail` pointer arms.
20. Doc truth-up pass: the four drifts in §2.7, plus delete the dead `focus` parameter.

---

## 5. What NOT to do

These were proposed and **refuted** by verification. They will be re-proposed by the next reviewer,
so they are recorded with the reason.

- **Do not convert the 172 `static mut` wholesale.** Verifiers checked: apart from `route.rs`'s two
  cross-thread reads, every one is main-thread-confined, and ~50 are write-once GL program ids and
  uniform locations. A blanket conversion is churn with no bug class eliminated. Fix the three that
  are actually reachable from a worker and leave the rest.
- **Do not introduce a uniform `Screen` trait to collapse app.rs's eight dispatch chains.**
  `docs/ui-viewtree-plan.md` records deliberate carve-outs — focused-last cross-row z-order,
  `player_hud`'s `SCR_H`-offset geometry shared with `app.rs`'s pointer hit-tests — that a uniform
  trait breaks. Extract the specific state machines instead.
- **Do not "finish" the retained view tree by driving it recursively.** Same invariants. Shrink the
  documented contract to what the code actually does.
- **The modal panel is not forked three times.** `ui/popover.rs` and `ui/table.rs` *are* the shared
  components, and all three menus use them. What repeats is instantiation glue.
- **Do not add a runtime ABI self-check for the FFmpeg struct offsets on the theory that a firmware
  bump could change them.** The stub SONAMEs (`Makefile:100-105`) pin `libavformat.so.57` /
  `libavcodec.so.57` / `libavutil.so.55`; a different major would fail to load, not mis-resolve.
- **`ff.rs` importing `route::stream_acodec` is not a layering violation** — the crate is flat,
  `ff.rs` is a sibling of `route.rs`. The *data race* is real (§2.3); the layering complaint is not.
- **The `catch_unwind`-poisons-a-mutex chain and the poster-cache-is-unbounded finding were both
  refuted** on mechanism. `posters.rs:179-188` is a real recency LRU.
- **The crash tracer's missing `SA_NODEFER` is real but low-value here** — a verifier reproduced it
  *on the live TV* and found the documented outcome is unachievable on this device regardless.
  Correct the doc, don't chase the code.

---

## 6. Quick wins (< 2 h each, most valuable first)

- [ ] `Makefile` as a prerequisite of the four build targets + assets in `$(RUST_LIB)`'s dep list.
- [ ] Clear the three `*_FETCHING` flags in `browse::reset()` (`browse.rs:171`) — one wedged screen gone.
- [ ] `n_hubs()` clamp at **all six** `shelves[]` index sites in `home.rs` — one panic class gone.
      Under-scoped as "four `hub_count()` sites" in the first pass; corrected by §8.c-C2. `Grid::vert`
      (`home.rs:564`, `:566`) indexes `shelves[cur]`/`shelves[ncur]` by the **focus row**, not by
      `hub_count()`, so a grep for `hub_count()` misses it — and it panics on the DOWN keypress,
      before `draw` is ever reached.
- [ ] `_Atomic int g_smp_ready` + release/acquire + a guard on `sf_feed` (`src/starfish.c:74`).
- [ ] `checked_add` in both `packet_to_annexb` passes (`ff.rs:1149`, `:1176`).
- [ ] `gfx::delete_tex` before zeroing `qr_tex` (`ui/login.rs:38`) and in `init()`.
- [ ] Add `"plxnative-anim.log"` to `DIAG` — or exempt any `.log` suffix (`app.rs:339`).
- [ ] `crate::log` instead of `eprintln!` at the six shader/font fatal sites.
- [ ] `rust-modules/.cargo/config.toml` with the target rustflags; fix `cr7, cr10` in `crash-report.sh:72`.
- [ ] `APP_FILES` shared by `deploy` and `ipk` so the ipk ships fonts.
- [ ] Fix the four doc drifts in §2.7 — they cost real debugging time.
- [ ] `text_width` → `TTF_SizeUTF8` (one `extern "C"` line, five call sites).
- [ ] Pass `env` to `Grid::hit_at` so the hit rect matches the drawn rect (`home.rs:587`).

---

## 7. Coverage & honesty about this review

- The fleet completed the **Map** (12/12 subsystems) and **Audit** (18/18 dimensions) phases and
  **271 of 355 adversarial verifications**. It then hit a session limit, so the *Design*,
  *Synthesize* and *Completeness-critic* phases never ran. §4's plan and §1's verdict are mine,
  synthesized from the verified findings — they were **not** put through the judge panel the other
  material was, and should be read as a strong proposal rather than a vetted design.
- **42 findings never got a verifier** and are excluded from §2/§3 above. Several look important
  and should be triaged before the plan is committed — notably: `[profile.release]` with LTO +
  `codegen-units=1` claimed to cut the resident image 5.2 MB → 1.7 MB (`Cargo.toml:22`); the
  deferred OK-press committing against the live route/focus (`app.rs:1866`); backgrounding mid-seek
  saving a stale `playpos()` so the seek is discarded on restore (`app.rs:789`); the sentinel-in-band
  class still live in `hero_button_at` (`home.rs:211`) — the same shape as the regression fixed in
  `125a828`; tokens and the login PIN in world-readable plaintext (`session.rs:126`); HTTP headers
  parsed as UTF-8 so one Latin-1 byte reports a good 200 as a connection failure (`stream.rs:191`);
  unvalidated image-subtitle rect geometry (`ff.rs:791`); and a claimed live defect in the
  home-catalog builder where shelves silently vanish (`pms.rs:284`).
- I independently re-verified, by reading the code myself: the two `route.rs` data races, the
  `put_selection` ordering bug, the unbounded `shelves[]` index, the glyph-cache key truncation, the
  `browse::reset()` wedge, the Makefile dependency gaps, the fontless ipk, the non-atomic
  `g_smp_ready`, the absent lint gate, and the two libc calls that block a host `cargo test`.
- **Not audited:** `tools/stream-screen.py` and `tests/run.py` internals beyond their interfaces,
  the `vendor/nanosvg` source, `src/gpdebug.c`, the `.claude/skills/` content, the stale `rust-poc/`
  tree, and the GLSL beyond the dead `u_focus` branch. Nothing was run on the TV for this review.

---

## 8. Addendum — the deferred verifications, closed

The 42 findings §7 listed as never-verified were each attacked by two independent adversarial
lenses; three completeness critics then reviewed the report itself. **20 survived both lenses, 22
did not.** Everything asserted below I re-opened and re-ran myself on this checkout (HEAD
`934db48`, tree clean). Where the two lenses disagreed on severity I take the lower and say so.
Severities here are the verifiers' corrections, not the original auditors'.

### 8.a Confirmed

#### high

**A1 — `cargo test` on the host is five edits away, and §4.17 names the wrong second blocker.**
`rust-modules/src/capture.rs:219` (`libc::__errno_location`) is the *only* compile error: I ran
`cargo check --lib` on macOS and got exactly one, across all 21,002 lines. `MSG_NOSIGNAL`
(`capture.rs:396`) is **not** a blocker — it resolves on darwin. The real second blocker is the
*link* step: the four `#[link(name = …)]` attributes at `ff.rs:175`, `:220`, `:255`, `:270` inject
`-lavformat -lavcodec -lavutil -lswscale` into the test binary, which dies with `ld: library
'avformat' not found`. They are redundant on device — `Makefile:54` already supplies all four on
the C link line and a `staticlib` is never linked by rustc; `net.rs:20` is the existing precedent
(a bare `extern "C"` for curl with no `#[link]`). With those five lines changed I built the test
binary and ran two real tests — `plex::models::de_i64` over int/string/bool/null/absent, and
`ff::adts_header` — both pass in 0.00 s.
I also measured the boundary the report does not state: a test reaching `gfx::frame_clear` fails
with `Undefined symbols … _glClear, _glClearColor`, so host testing covers the **pure half only**
(`plex/` builders + deserializers, `route`'s URL/decision logic, `ff.rs`'s byte transforms, `aq`,
`cbuf`, text wrap/elide, `fmt`); anything touching SDL/GLES/TTF/curl/ACB/Starfish stays
device-only. And the host is 64-bit aarch64, so it cannot validate the FFmpeg 3.3 struct offsets
(32-bit armv7 layout) — strike `ff.rs`'s *ABI* from item 17's target list, keep its transforms.
Prefer `#[cfg_attr(target_os = "linux", link(name = …))]` to deletion: the FFI sites keep
documenting their libraries and the ARM link is byte-identical.
*Fix:* one `cfg` at `capture.rs:219` + four `cfg_attr`s in `ff.rs`. → **Wave 0** (the unblock;
the tests themselves stay item 17)

#### medium

**A2 — the Info-card "go to detail" jump skips the hub refresh, and the stale card then *plays* at
the stale offset.** `app.rs:989-998` performs three of `exit_player`'s four effects and drops
`*refresh_hubs_at` (`app.rs:614`) — whose own doc comment at `app.rs:606-609` reads "A new exit
path that skips this quietly re-introduces the stale-CW bug." `refresh_hubs_at` is armed only by
`exit_player` (`:1224`, `:1294`, `:1726`) and is the only post-playback hub refetch in the crate.
The impact is worse than a stale bar: `home_activate` → `play_item_now` (`app.rs:620-636`) resumes
from `mm.resume_ms` in the un-refreshed catalog, so pressing OK on that Continue-Watching card
replays from the pre-session offset. *Fix:* give `exit_player` a `dest: Route` parameter and call
it from this arm. (Lenses split medium/high.) → **Wave 2**

**A3 — Login and Profiles are absorbing states; the one escape function is dead.** The auth-phase
follower at `app.rs:1888-1903` re-derives `route` from `auth::phase()` **every frame** while in
`Login | Profiles`, and its `_ =>` arm catches both `Phase::Idle` and `Phase::Error`
(`auth.rs:19`, `:34`) → `Route::Login`. Fresh keys on those two routes are dispatched to the screen
and `continue` at `app.rs:903-918`, before the BACK handler; `ui/login.rs:143-147` handles only
OK-on-Error ("no exit until sign-in completes") and `ui/profiles.rs:438` says "BACK on the picker
does nothing". `auth::cancel()` (`auth.rs:150`) — written for exactly this — has **zero callers**
(silent because `auth.rs:10` is `#![allow(dead_code)]`). Reachability is narrower than the finding
claimed: `account_menu.rs:38-46` already gates "Change profile" on a live session, with a comment
recording a sibling dead end. What remains reachable is `start_switch`'s background roster refresh
failing while the *persisted* roster is empty (`auth.rs:215-219` → `set_error` → `Phase::Error`),
which pins the user on a Login screen that swallows BACK. *Fix (~10 lines):* remember that the flow
was entered from Home at the two account-menu sites, and map `Error`/`Idle` back to `Route::Home`
in that case only — the boot gate keeps its documented no-exit rule. (Lenses split high/medium.)
→ **Wave 2**

**A4 — pointer input treats only the track menu as modal.** The key path treats all three player
overlays as modal, each with an explicit `continue` and the comment "swallows every key while
open" (`app.rs:948`, `:967`, `:1014`). The click arm gates on `Route::Player { .. }`
(`app.rs:1358`) and special-cases only `Overlay::Menu` (`app.rs:1365`); `Info` and `Chapters` fall
through to `icon_hit` → opens the track menu *under* the card, `scrub_hit` → a blind seek, else a
pause toggle. `info_panel.rs`/`chapters_panel.rs` expose no pointer entry point at all. This is
also reachable from the project's own headless driver (`ck:X,Y`). *Fix (~6 lines):* hoist modality
onto `Overlay` — any non-`None` overlay dismisses via the existing `close_player_overlays()`.
→ **Wave 2**

**A5 — the player HUD's focus survives across playback sessions.** `hud_focus`/`hud_btn`/`hud_tab`
are `plex_run` locals (`app.rs:482-484`) with no owning module. `start_playback`
(`app.rs:579-596`) resets none of them, and the only general reset (`app.rs:1829-1833`) is gated on
`route == Player && !hud_shown(…)` — false for the whole `HUD_LINGER_MS` window that
`start_playback`'s own `set_hud` opens. `app.rs:1727` zeroes `hud_focus` on the EOS path only, so
Stop (`:1224`), BACK (`:1294`) and the background suspend (`:787-800`) all leak. With
`hud_focus == 1` carried in, the first OK of the new movie opens the track menu (`app.rs:1124`)
instead of pausing, and LEFT/RIGHT walk buttons instead of seeking (`app.rs:1242`). *Fix
(minimum, 4 lines):* reset the three in `start_playback`. → **Wave 2** (the full `HudFocus`
module belongs to item 19; `docs/ui-system-migration.md` §D.3 defers the transport machine
deliberately, and `hud_focus` is coupled to the scrub locals at `app.rs:872`, `:1076-1082`)

Two findings landed on rows §2.7/§3 already carry, and both are **confirmed as written**, each
with one delta:
- `ui/CLAUDE.md:84`'s "one user" (§2.7) — confirmed; the second user is deliberate and the
  perf half of the original claim is refuted (`tests/manifest.json`'s `home-grid` and
  `library-scroll` FPS gates already exercise the resume-bar path and pass). Doc truth-up only.
- `ff.rs:1149`/`:1176` 32-bit wrap (§3) — confirmed at medium. Delta worth folding into Wave 2
  item 13: `AV_PKT_FLAG_KEY` is declared at `ff.rs:295` and **read by nobody** (verified
  tree-wide), while `packet_to_annexb`'s return value is the app's sole keyframe oracle
  (`ff.rs:1458` → `aq_push` `:1481` → the post-seek rebase gate at `engine.rs:625`, and the
  drop-forever path at `:672`). OR in libavformat's own flag so a walk that breaks early still
  reports the right answer.
- `text.rs:89`/`:239` glyph-key truncation (§3) — confirmed at medium by both lenses.

#### low

**A6 — the boot picker is suppressed by files the app itself writes, and by three diagnostics the
allow-list forgot.** `app.rs:339`'s `DIAG` is seven *exact names*. `ui/anim.rs:109` creates
`/tmp/plxnative-anim.log` — not on the list — so arming the DIAG-exempt anim overlay once marks
every later boot automated. Both sanctioned clear paths spare `*.log` (`tools/tv-session.sh:79`,
`tests/run.py:188`) and `tv-session.sh:233`'s `status` filters `*.log` out of its listing, so the
file is unclearable by the tools and invisible to the tool that exists to show it. §2.7 has this
much. Wider than the report states: `plxnative-novsync` (`app.rs:285`), `plxnative-framedrop`
(`app.rs:450`) and `plxnative-ffprobe` (`ff.rs:840`) are also pure diagnostics and also absent from
`DIAG`. *Fix:* one line — `.starts_with("plxnative-") && !n.ends_with(".log") && !NAMED.contains(…)`
— which makes the app agree with the rule `tv-session.sh` and `run.py` already implement in three
places. The `triggers.rs` catalog stays the Wave-3 version. → **Wave 0**

**A7 — `-Iinclude` is dead, and `CLAUDE.md:48-50` is not merely stale, it is backwards.**
`Makefile:38` still passes `-Iinclude` and `Makefile:46-47` still explains why. But the three
compiled TUs (`Makefile:66`) include no SDL/GLES header, transitively or otherwise (`main.c:6-12`,
`starfish.c:5-7`, `svg.c:8-15`; `app.h:3` pulls only `<stdio.h>`, `starfish.h` pulls nothing,
`vendor/nanosvg` pulls only libc). More important, the direction of the guarantee is inverted:
`include/SDL2/SDL_events.h:187-197` is **stock** SDL 2.0.4 (`SDL_version.h:60-62`) with no
`inputSource` field, while the NDK sysroot's copy at `SDL_events.h:222-233` carries
`Uint32 inputSource; /**< webOS specific field */` — which is precisely what puts state at +16 and
sym at +24, the offsets `app.rs:831-834` reads and `CLAUDE.md:139` orders preserved. The sysroot
also ships `SDL_webOS.h`; `include/` does not. So `-Iinclude` was shadowing the *correct* fork
headers with stock ones that lie about the runtime layout, and its being dead is an improvement.
*Fix:* drop the flag, delete the `Makefile:46-47` comment, rewrite `CLAUDE.md:48-50` to credit the
NDK sysroot, and delete `include/`. Reject the finding's second half (a host-side header-vs-extern
auditor): its oracle would be the stock headers that disagree with the device. → **Wave 0**

**A8 — the C toolchain is pinned and the Rust toolchain floats.** `Makefile:113` pins
`NDK_REL ?= webos-d7ed7ee.6`; `Makefile:90` is a bare `cargo +nightly build -Z build-std=…`. There
is no `rust-toolchain.toml` and no `.cargo/config.toml` anywhere in the tree, and `rust-src` — a
hard prerequisite of `-Z build-std` — exists only as prose (`CLAUDE.md:27`,
`.claude/skills/setup-environment/SKILL.md:45-46`), so a fresh machine fails inside cargo. The
installed nightly is `1.98.0-nightly (c397dae80 2026-07-02)`; **do not** use the
`nightly-2026-06-26` in the original finding — that string is a stable `rustc --version`, misread.
Two corrections to the finding's rationale: the CP15/SIGILL behaviour is governed by
`-C target-cpu=cortex-a9` (`Makefile:87`), which *is* pinned; and a codegen regression is already
detectable (`crash-triage/SKILL.md:70-73` checks CP15 count 0 and `Tag_CPU_arch: v7`) — what is
missing is a rollback point and the `rust-src` auto-install. Keep
`PATH="$$HOME/.cargo/bin:$$PATH"` at `Makefile:89`; it locates cargo and has nothing to do with
channel selection. → **Wave 0**, alongside item 2's `.cargo/config.toml`

**A9 — the root `CLAUDE.md` undercounts the playback workers, omitting the one that causes §2.3's
second race.** `CLAUDE.md:97` says "Two worker threads (demux, media/load)". `engine.rs` spawns
three — `:294` `ff::demux`, `:305` `load_thread`, `:315` `timeline_thread` — and joins three
(`:486-494`). The third is the ~10 s timeline reporter, and it is exactly the thread that reads
`route.rs`'s unsynchronized statics (`threads.rs:51` → `route.rs:642-652` → `sess()`, `pq_id()`,
`cur_audio_sid()`, `cur_sub_sid()`). Reword rather than expand; the spawn is conditional on
`stream && !cur_rk().is_empty()`. Do **not** add the finding's second half to
`player/CLAUDE.md` — `timeline_thread`'s own cross-thread state is entirely in `shared.rs`/`TX`,
and `threads.rs:23-26` documents the deliberate `rk`-at-spawn hoist, so the suggested note would
record something untrue. → **Wave 3**, item 20

**A10 — `ui/consts.rs` claims to be the one home for the remote wcodes; `CLAUDE.md:140` says so
too; neither is true for the D-pad.** `consts.rs:42-46` names `WCODE_PAUSE`/`STOP`/`PLAY`, and
`CLAUDE.md:140` states "D-pad L/R alt 412/417; the wcodes live in `ui/consts.rs`". They do not:
`412` and `417` appear only as bare literals in `app.rs`, in a six-term predicate copy-pasted
verbatim at `:835`, `:1015` and `:1226` plus two direction half-copies at `:1016` and `:1236`;
`415` (`:1194`) and `19`/`402` (`:1201`) sit inline beside the *named* `WCODE_PAUSE`/`WCODE_PLAY`;
`0x1e4` (`:1119`) is an unnamed pointer-lifecycle code. *Fix:* named constants plus
`is_left`/`is_right`/`is_play`/`is_pause` beside the existing `is_ok`/`is_back`. One caveat the
finding omits: `app.rs:1022` must keep its explicit `sym == SDLK_LEFT || sym == SDLK_RIGHT` test —
the comment at `:1019-1021` records a device-verified reason (arming hold-repeat with a normalized
key sticks on release). → **Wave 3**, item 20

**A11 — `remote.rs:66` panics on a multi-byte whitespace character.** `self.buf.rfind(char::is_whitespace)`
returns the *start* byte of the match, so `buf[..=i]` / `buf[i+1..]` (`:68-69`) slice inside any
non-ASCII whitespace. The FIFO is drained every frame on every boot (`app.rs:755-771`), the panic
crosses the `extern "C"` `plex_run` boundary and aborts. Exposure is dev-only: `tools/stream-screen.py`
allowlists every token and both shipped writers use `printf '%s\n'`, and the trailing newline makes
the last whitespace ASCII — so only a hand-typed `printf 'ok\xc2\xa0'` with no newline trips it.
*Fix:* `rfind(|c: char| c.is_ascii_whitespace())`. Every token is ASCII, so nothing is lost.
→ **Wave 2**

**A12 — HTTP response headers are parsed as strict UTF-8.** `stream.rs:191` is
`std::str::from_utf8(&hs.buf[..hdr_end]).unwrap_or("")`, so one non-UTF-8 header byte collapses the
whole block to `""`, `hs.status` stays 0, and `stream.rs:208-211` closes the socket and reports a
good 200 as a transport failure. `stream.rs:193` then byte-indexes `hdr[9..]`, which panics if a
multi-byte char spans index 9. Both lenses agree the code is as described; both also refuted the
Content-Disposition trigger (`docs/plex-openapi.json` documents it as download-only and this client
issues no download requests), so this is hardening, not a live defect. *Fix:* `from_utf8_lossy` +
a byte-oriented digit scan — closer to RFC 7230's "opaque octets" and it removes both at once.
→ **Wave 1**, folded into item 6 (which already surfaces the status)

**A13 — backgrounding mid-seek saves a stale `playpos()`.** `app.rs:789` is a bare
`bg_pos = playpos();`. The other two readers of the playhead both apply the in-flight rule —
`app.rs:1255-1268` ("If a prior commit's seek is still landing, playpos() is stale") and
`ui/player_hud.rs:249-251`. `suspend_bufferfeed` → `teardown(true)` → `reset_session()`/`TX.reset()`
wipes the pending target, so nothing self-corrects and the restore resumes at the pre-seek spot.
The exposed window is one frame on the common path, widening only when the pump coalesces behind an
unresolved seek (`pump.rs:84-96`). Drop the finding's second impact claim: `bg_pos` is both the
restore target and the `app.rs:1844` gate's reference, so a stale value stays self-consistent.
*Fix:* hoist one `player::intended_pos_ns()` beside `playpos_ns` (`player/mod.rs:64`) and call it
from both sites. (Lenses split medium/low.) → **Wave 2**

**A14 — the poster store key is truncated at 255 but looked up at full length.**
`Pslot.key` is `[u8; 256]` (`posters.rs:35`) written through `cbuf::set_bytes_raw`, which caps at
`dst.len()-1` = 255 (`cbuf.rs:16`); the lookup at `posters.rs:163` compares against the full
`key_s.as_bytes()` built into a `[0u8; 352]` (`posters.rs:142`, `ui/widgets.rs:19`) — and
`ui/widgets.rs:17` stages the *source* path through a second `[0u8; 256]` first. Three capacities
that must agree, spelled three ways. Latent, not live: both lenses measured real keys against the
live PMS at 132-162 bytes, ~93 bytes of headroom. *Fix:* one `POSTER_KEY_CAP` const owning all four
sites, and make `cbuf` truncation loud. The same class applies to `route.rs:502-503`
(`TITLE` 128 / `CTXLINE` 96, byte-wise, no char-boundary check). → **Wave 2**

**A15 — poster/artwork decode has no dimension or allocation budget.** `img.rs:21-26` calls
`image::load_from_memory`, which uses `Limits::default()` — verified in the vendored
`image-0.25.10/src/io/limits.rs:49-57`: `max_image_width: None`, `max_image_height: None`,
`max_alloc: Some(512 MiB)`, on a device declaring `requiredMemory: 60` (`pkg/appinfo.json:14`).
The existing `catch_unwind` (`img.rs:21`) catches decoder *panics* but not an allocation failure,
which aborts — so the module's own "never crashes the app" promise (`img.rs:2-3`) has a hole.
Unreachable from the legitimate path (every fetch is a `/photo/:/transcode` with explicit
width/height, largest 1920x1080), so this is hardening against a hostile or broken PMS. *Fix:*
build the reader explicitly and set `max_image_width/height = Some(4096)`,
`max_alloc = Some(16 << 20)` on a mutated `Limits::default()` (the struct is `#[non_exhaustive]`,
so a struct literal will not compile). Drop the finding's `stream::http_get` `max_bytes` and
`poster_worker` `catch_unwind` halves — neither is reachable. → **Wave 2**

**A16/A17 — two host-testable surfaces to name explicitly under item 17.** `plex/transcoder.rs:68`'s
`transcode_query` and the other three URL builders, plus `client.rs:176`'s `QueryBuilder`/`enc` and
`models.rs:334`'s lenient `de_i64`/`de_f64`, are pure and unasserted — though the *session*
invariant at `transcoder.rs:66-68` is enforced by construction (one private producer feeding both
`:130` and `:135`), so testing it is tautological; the value is in the deserializer's known-bad
shapes. And `ff.rs`'s five byte transforms (`parse_extradata` `:1011`, `is_hdr10plus_sei` `:1103`,
`packet_to_annexb` `:1133`, `adts_freq_index` `:1204`, `adts_header` `:1243`) are pure and pin the
ADTS/Annex-B bit packing that `tests/run.py` cannot see — its only audio assertion is a liveness
grep (`run.py:364`). Both are now unblocked by A1; I ran one test from each family. → **Wave 3**,
item 17

### 8.b Refuted

Recorded so the next reviewer does not re-derive them. Read with §5.

- **`Painter::clip` needs a scoped `ClipGuard`.** No composition in the tree nests a card inside a
  clip (`TableView`'s `Row` is a closed struct with no caller-supplied draw), and the proposed
  guard restores on exit but still *replaces* on entry, so it would not fix the hazard it names.
- **`detail::draw_strip` is the general two-pass strip loop.** Its doc scopes it to detail's
  `CardRow` shelves. `library.rs` has no `CardRow` (two-spring model for unbounded grids,
  `library.rs:275-276`) and interleaves ~38 lines of top chrome between its two passes on purpose
  (`library.rs:872`). One marginal adopter (`profiles.rs`), needing a 13th parameter.
- **The three menu screens fork a modal panel.** `ui/popover.rs` and `ui/table.rs` are the shared
  components; `hit_row` has exactly one implementation (`table.rs:117`) with four call sites, and
  the `MENU_BUILT` guard is library-only because only library's rows come from background threads.
- **`route.rs:177`'s discarded `/decision` breaks the session contract.** A verifier probed the
  live PMS: a seek on an already-registered session returns 200 + valid Matroska without a second
  decision (4/4). Residual, low: `route.rs:426` and `:572` swallow a `None` with no log line while
  `ff.rs:1272` logs only the consequence.
- **`[profile.release]` LTO cuts 3.5 MB of *resident* image.** Measured and real as an *on-disk*
  cut (7.2 MB → 2.1 MB, `.text` 5,050,640 → 1,581,693), but `.text` is file-backed and
  demand-paged, and the removed code is dead/duplicated. It also drops FUNC symbols 10,672 → 2,526,
  degrading `tools/crash-report.sh` — the only symbolication path, which has no DWARF in release
  and calls `addr2line` without `-i`. Worth doing for deploy speed, bundled with that mitigation.
- **The build needs a stub-symbol/device ABI gate.** `.claude/skills/bind-tv-lib-abi/SKILL.md` is
  that gate, by design, and all 21 avcodec stub symbols probe PRESENT today. Wiring it into
  `deploy` puts an ssh/cache dependency on `make test`.
- **`make clean` must also wipe `rust-modules/target`.** 3.5 GB and `-Z build-std` means the next
  build recompiles std; `cargo clean` already exists and is what the in-repo setup skill prescribes.
  Residual: add `stub/*.so` to `clean`. (`rm -f src/*.d` is dead — no depfiles are generated.)
- **`PMS_HOST` compiled into the binary is an exposure.** `pkg/plxnative` and `pkg/*.ipk` are
  gitignored and never distributed, and the tracked `Makefile:23-25` already commits the TV's LAN
  IP and root password as a documented choice.
- **`pms.rs:284`'s unlabelled `break` silently drops shelves.** Mechanism real (the inner `break`
  leaves later shelves failing the `new_cat.len() > start` guard) but the 256 cap is unreachable:
  `home_hubs(12)` bounds a hub at 12 items and a verifier measured the live server at 41/256.
  `break 'outer` + a log changes nothing a user sees.
- **`pick_dp_audio`/`build_stream` are untestable and untested.** All 18 `tests/manifest.json`
  cases assert `expect.decision` end-to-end (`run.py:465`), with a named anchor case per cited fix.
  The eager-`server` variant would force a `/decision` round-trip on the direct-play fast path
  `route.rs:379` deliberately short-circuits.
- **`gfx.rs:460` `spring_zeta` NaNs at `k = 0`.** True and unreachable: every `k` at all 21 call
  sites is a nonzero compile-time constant, `spring_zeta` is `pub(crate)` with one caller
  (`press.rs:194`, `K_UP = 340.0`), and `Spring::jump()` is the idiom for snapping.
- **Injecting a measurer into `elide_compute`/`wrap_uncached` unlocks host tests.** It does not:
  `wrap_uncached` calls the memoised global `text::elide`, which drags TTF+GL back in. The
  historical defects in that code were performance, invisible to a stub measurer.
- **`plex_run`'s 1,830-line `unsafe` block makes input untestable.** Per-screen input rules are
  already named `pub(crate)` functions outside it, and neither cited regression originated there
  (`211b834` was a device-latency threshold in `pump.rs:60`; `125a828` was `ui/home.rs:194`).
- **The 172 `static mut` gate testability.** They do not; the crate's host-buildability and its FFI
  surface do (A1). 28 of the 172 are `&'static mut` return types, not declarations.
- **The deferred OK-commit activates the wrong card.** `press::scale()` multiplies whichever tile
  is focused *this frame*, so the dip tracks focus and visual never diverges from activation. The
  real residual is that `ok_armed` carries no route, reachable today only through the dev FIFO —
  ~3 lines beside `app.rs:510`, not a `PressTarget` enum, and any pointer cancel must go **below**
  the `mot_accum` gate at `app.rs:1333`.
- **`hero_button_at`'s sentinel namespace is a four-way architecture defect.** Both consumers guard
  `b >= 0` and `set_hero_focus` clamps, so nothing misroutes; `hero_pill_index` already centralises
  the packing by design (`125a828`). Residual: return `Option<usize>` from `home.rs:211`, ~6 lines.
- **`library::enter` silently no-ops on zero sections.** Unreachable: `install_pms` runs
  `ensure_sections()` before Home is interactive (`app.rs:323`), and with zero sections
  `set_hero_focus`'s clamp and the `i > 0` pointer guard both close the entry.
- **Eight non-exhaustive route chains need a `Screen` trait.** Already refuted in §5 on different
  grounds; additionally, `held_sym` is provably 0 on Account/Profiles (all four arming sites sit
  behind the `continue`s), so the missing match arms would be dead code.
- **`browse.rs:481`'s `resize_with(totalSize)` is a live OOM.** `browse.rs:202-206` admits only
  `movie`/`show` sections, so episode-scale counts are unreachable, and `requery()`/`reset()` clear
  the store. Residual, low: a 2-line clamp at `browse.rs:566`.
- **`seek_cb`'s missing `206` check silently corrupts the demux.** No proxy is reachable —
  `stream.rs` connects to a numeric LAN IP with no DNS and no redirect handling — and the Cues
  index for *every* direct-play open rides this path, so a 200-instead-of-206 would break all
  playback visibly. (A different, live defect in the same function survives; see 8.c.)
- **Tokens and the login PIN are world-readable.** The PIN half is wrong: `auth.rs:245`'s code is
  rendered on-screen at `size::HERO` beside a QR that encodes it — a public one-time linking code.
  The `session.rs:126` half is real but at-rest hardening on a rooted single-user appliance; a
  verifier found the file is already 0777 in a 0777 directory alongside a 0777 root `.ssh`. Take
  the tmp-file + `rename` for atomicity; skip the framing.
- **`ff.rs:791`'s bitmap-subtitle geometry allows unbounded reads.** Bounded by the
  `stride >= w` guard plus the `AVSubtitleRect` contract and `u8` palette indices; the 32-bit wrap
  needs `w*h >= 2^30`, which the decoder cannot have allocated first. Residual, low: a dimension
  sanity bound and a log on the currently-silent discard branch.

### 8.c What the first pass missed

Only critic-raised gaps I re-verified myself by reading code. Speculative items dropped.

**C1 — `stream.rs` sends without `MSG_NOSIGNAL` and the process has no `SIGPIPE` disposition.**
`stream.rs:151` is `libc::send(fd, …, 0)` — the only place the app writes a PMS request. A
tree-wide grep for `SIGPIPE`/`SIG_IGN`/`MSG_NOSIGNAL` returns the sibling that got it right
(`capture.rs:396`, whose doc at `:390` says outright "SIGPIPE would kill the app") and nothing else.
The usual net does not exist: `main` is C (`src/main.c:92`) calling `plex_run` as a plain
`extern "C"` (`app.rs:248`), so Rust's `std::rt::init` — which installs `SIG_IGN` for SIGPIPE —
never runs, and `install_crash_tracer` (`src/main.c:80-90`) registers SEGV/ABRT/BUS/ILL/TRAP only.
A peer that closes between `connect` and the request write therefore terminates the process with
**no log line at all** — not even the crash tracer. *Fix:* one line in `src/main.c`
(`signal(SIGPIPE, SIG_IGN)`), which covers every socket in the process, plus `MSG_NOSIGNAL` at
`stream.rs:151` to match `capture.rs`. **medium** → **Wave 0**

**C2 — §4.14/§6's "clamp the four `hub_count()` sites" is under-scoped; the site that panics first
has no clamp at all.** `MAX_HUBS = 16` (`home.rs:88`), `shelves: [CardRow; MAX_HUBS]` (`:412`).
`update` (`:419`) and `layout` (`:437`) iterate `0..MAX_HUBS` and `env` (`:638`) clamps with
`.min(MAX_HUBS - 1)` — safe. `draw` (`:442`) and `hit_at` (`:577`) iterate `0..hub_count()`
unclamped, which is the §3 finding. **But `Grid::nav` (`:548`) bounds DOWN with a raw
`hub_count()`, and `Grid::vert` indexes `self.shelves[cur as usize]` (`:564`) and
`self.shelves[ncur as usize]` (`:566`) with no bound whatsoever, then writes the unclamped result
back to the focus global at `:569`.** With 17+ hubs, DOWN from row 15 panics inside `vert` before
`draw` is ever reached. The accessor must cover six sites, not four. **high** → **Wave 0**
(alongside the existing quick win)

**C3 — the crash tracer is not async-signal-safe, and the event log has two uncoordinated writers.**
`src/main.c:24` states the handler "must stay minimal/async-signal-safe"; `write_trace`
(`main.c:37-56`) calls `fprintf`, `fopen("/proc/self/maps")`, `fgets`, `sscanf`, `fclose`,
`fflush`. A SIGABRT raised from inside malloc — heap corruption, the case the tracer exists for —
deadlocks on the arena lock in `fopen`, so the app hangs instead of dying: no re-raise, no crashd
backtrace, no SAM `exit_status`. Separately, `main.c:94` opens the event log with
`fopen(…, "w")` — a stdio stream carrying its own offset — while `lib.rs:32` opens the same path
`.append(true)` per line. The C stream writes only a handful of lines (`starfish.c:123`, `:142`,
`:158`, plus `write_trace` at crash time), so its offset stays near zero and each `fprintf`
overwrites the **head** of the Rust-written log — including, at crash time, exactly the boot lines
you need. *Fix:* have `write_trace` use `write(2)` into a pre-opened fd with a preformatted buffer,
and give the C side the same append-per-line sink as Rust (or route `starfish.c` through
`crate::log`). **medium** → **Wave 1**

**C4 — three of the four FFmpeg stub headers name the wrong FFmpeg release, and §5's ABI refutation
rests on them.** `stub/avformat_stub.c:1`, `stub/avcodec_stub.c:1` and `stub/avutil_stub.c:1` each
say "(FFmpeg 3.4)"; `ff.rs:1-4` pins n3.3 (57.71.100 / 57.89.100 / 55.58.100) and the ~93
layout-bearing items in `ff.rs` are derived from that point release. `stub/swscale_stub.c:1`
already says 3.3 correctly. §5's refutation ("the stub SONAMEs pin the version") is right about the
*major* and those SONAMEs span several minors — so the comment is the only version statement in
`stub/`, and it is wrong. This does not resurrect the runtime self-check; it is a fifth row for
§2.7's drift table. **low** → **Wave 0**

**C5 — `make deploy` can never update a font.** `Makefile:136-137` guards both font copies with
`$(SSH) 'test -f …' || $(SCP) …`, so once `appfont.ttf` exists on the TV a changed font is never
deployed (same for `libturbojpeg.so.0` at `:139-140`). The report found the *ipk* fontless (§2.6);
the dev loop cannot refresh the very asset the pixel-snapping/light-hinting contract and
`tools/font-hint-audit.py` are built around, so a font swap silently verifies against the old
face. *Fix:* fold into the `APP_FILES` list Wave 0 item 3 already introduces, and compare md5
rather than existence. **medium** → **Wave 0**

**C6 — `http_open`'s memset opens a `close(0)` window, which is the sharp version of §2.4's
`close()`-as-interrupt row.** `fd` is the **first** field of `HttpStream` (`stream.rs:10`);
`http_open` memsets the whole struct (`stream.rs:94`) and only then writes `hs.fd = -1`
(`stream.rs:96`), so `fd == 0` for the duration of a 64 KB memset. The demux thread re-enters
`http_open` on every reopen (`ff.rs:968`, `:1270`) while the main thread calls `http_close` on the
same struct at `pump.rs:75`, `pump.rs:146` and `engine.rs:482` — and `pump.rs:75`'s own comment
("re-interrupt: the first close raced the reopen") says the retry path deliberately re-races it. A
hit runs `libc::close(0)`. `stream.rs:376-382` documents this exact hazard and guards only the
*allocation* path. It matters because `crate::log` (`lib.rs:32`) opens and closes the event log
**per line**, so fd 0 is a live candidate to be recycled and then closed by the next seek. The
`fd → AtomicI32` half of Wave 2 item 10 fixes it; initialise to `-1` before the memset, or memset
only the payload. **medium** → **Wave 2**, item 10

**C7 — `capture.rs` already implements the pattern Wave 2 item 10 designs from scratch.** Fds as
`AtomicI32` (`capture.rs:74-76`), `shutdown(SHUT_RDWR)` rather than `close` to interrupt, with the
rationale written down (`capture.rs:107-109`: "`shutdown(2)` — not `close` — wakes a blocked
`accept()`/`send()` on Linux; each thread then closes its own fds"), and `MSG_NOSIGNAL` on every
send. `stream.rs` is the outlier, not the frontier. Item 10 is therefore a port, not a design —
cheaper and lower-risk than the report implies. *(No severity: this reduces an existing item's
cost.)*

**C8 — do not schedule `SEEK_STUCK_MS`'s removal behind item 10.** §2.4 says the 1200 ms watchdog
"exists because the wrong syscall was used." `pump.rs:56-59` records the opposite half: a *shorter*
watchdog self-DoS'd the rapid-tap seek path and was caught by the `seek_rapid` harness cases. And
`pump.rs:64-77` shows the watchdog doing work `shutdown(2)` cannot — it adopts the newest coalesced
target from `TX.seek_to_ns` and **re-arms** `SHARED.next_url`/`seek_byte`/`seek_to_ns` for the
demuxer's outer loop. The fd fix and the watchdog are independent; strike "then reconsider
`SEEK_STUCK_MS`" from item 10. *(Sequencing correction.)*

**C9 — the `seek_cb` guard must also refuse `s.size < 0`, not merely require 206.** `seek_cb`
(`ff.rs:953-957`) answers `AVSEEK_SIZE` with `s.size` and computes `SEEK_END` targets from it
(`:962`) — and `s.size` is `-1` on a live transcode, which `pump.rs:87-88` states explicitly ("A
live transcode has NO Content-Length (file_size stays -1)"). Meanwhile `route.rs:154-158` records
that a transcode has no byte-Cues so a byte-Range seek cannot work, yet `ff.rs:1293-1301` installs
`Some(seek_cb)` unconditionally, which is what sets `AVIO_SEEKABLE_NORMAL` on that stream.
Wave 2 item 15 should add the `s.size < 0` refusal (or not register `seek_cb` for a transcode
source) alongside the 206 check. **medium** → **Wave 2**, item 15

**C10 — the text pipeline is single-face with no glyph fallback, so CJK titles are unrenderable.**
`text.rs:15-17` hardcodes one face pair; `font_at` (`text.rs:129-150`) opens exactly one
`TTF_OpenFont` per (size, bold) and its two fallbacks fire on *file-open* failure, never on a
missing glyph. I parsed both bundled faces' `cmap`: 154 segments, max mapped codepoint `0xFFFC`;
Latin, Latin-ext, Cyrillic, Greek, Arabic and Hebrew are covered, and **U+4E2D, U+65E5, U+3042 and
U+D55C are not** — in `appfont.ttf` and `appfont-bold.ttf` alike. Every anime / HK / K-drama title,
cast name and synopsis in an ordinary Plex library therefore paints as .notdef, and the
server-driven A-Z rail will offer letters whose titles cannot be drawn. *Fix:* a per-glyph
coverage test feeding a second face; the device's own `/usr/share/fonts` is the obvious source, but
which files exist there is **unverified** (not checked on-device for this review). Note this widens
the rasterization contract: `tools/font-hint-audit.py`'s guarantee about the `theme::size` ladder
holds only for the scripts the bundled face covers. **medium** → **Wave 3**

**C11 — boot presents no frame at all until two blocking PMS GETs return.** `install_pms`
(`app.rs:316-325`) runs `pms::pms_fetch_hubs()` (`:321`) and `browse::ensure_sections()` (`:323`)
synchronously, and is called at `app.rs:373` (dev token) and `:386` (stored session) — both
*before* the event loop; the first `SDL_GL_SwapWindow` is at `app.rs:1993`/`:2030`, inside it.
Combined with the confirmed absence of a connect deadline (§2.4, `stream.rs:122-126`), the boot
case against an unreachable PMS is not "a frozen UI" — it is a window that has never had content,
for the kernel's full SYN-retry budget, on the exact path SAM watches to decide the launch
succeeded. *Fix:* swap one frame before any socket is opened, and put the boot catalog fetch behind
the `Load<T>` state item 7 already introduces. **medium** → **Wave 1**, with item 9

**C12 — there is no buffer-health concept: a network stall has no state, no spinner and no log
line.** `engine.rs:32-33` caps the queues in *bytes* (`AQ_VIDEO_BYTES = 8 MiB`,
`AQ_AUDIO_BYTES = 1 MiB`), so jitter tolerance varies roughly 10x between a 10 Mbit/s 1080p item
and a 60 Mbit/s 4K remux — across exactly the library the direct-play path exists to serve. A grep
for `underrun|rebuffer|starv|buffering` across `player/` returns only the Load-payload JSON and two
comments about audio priming. `player::loading()` (`player/mod.rs:79`) — the only thing that raises
the HUD spinner — is `SHARED.seeking`, set exclusively by `request_seek`. A Wi-Fi hiccup longer
than the queue therefore freezes the picture with no indicator and nothing in the event log.
*Design note for item 5:* `SHARED.fault` needs a non-fatal sibling — a starvation state (queue
empty while `!paused && !eof`) driving the existing spinner plus one log line. **medium** →
**Wave 1**, item 5

**C13 — backgrounding mid-transcode orphans the PMS encoder, even on a clean exit.**
`suspend_bufferfeed` → `teardown(true)` (`engine.rs:445-447`) deliberately skips the `stopped`
scrobble and `route::stop_transcode()` (`engine.rs:519-525`) so the foreground reload is clean.
But `teardown` also calls `TX.reset()` (`engine.rs:522` → `shared.rs:200`), which clears `started`
— so at process exit `app.rs:2072`'s `if is_started()` is false, `stop_bufferfeed()` is skipped,
and `route::stop_transcode()` (whose **only** caller is `engine.rs:524`) never runs. A user who
backgrounds mid-transcode and never returns leaves a live encoder and an item stuck in Now Playing,
with the resume point frozen at the last 10 s reporter post. *Fix:* send a Paused timeline +
`stop_transcode` on background, or make the exit path call `stop_transcode` unconditionally when
`TSESSION` is non-empty. (The critic's separate SIGTERM claim is **not** included: `app.rs:783-784`
does handle `SDL_QUIT`, and whether SDL's fork posts it on SIGTERM is unverified.) **medium** →
**Wave 2**

**C14 — `ui/widgets.rs:337`'s row in §3 is factually wrong: there is a third tab row, and it uses
the estimator.** `ui/player_hud.rs:327` calls `TabPill::width(label.chars().count(),
theme::size::BODY)` — a codepoint count times a Latin advance ratio. The report states "both real
tab rows bypass it and measure with `text_width`". Two do; the player HUD's does not, which is also
the row most likely to carry a non-Latin label. **low** → **Wave 3**, item 20

**C15 — two smaller items, verified but low-value.** (a) `auth::retry()` → `start_login()`
(`auth.rs:137-147`) unconditionally spawns another `login_thread`; the only cancellation is the
cooperative `phase != Waiting` check inside a 2 s sleep (`auth.rs:316-318`), so a re-entry whose
new thread reaches `Waiting` inside that window leaves the old thread polling a dead pin for up to
30 minutes — and `auth::cancel()`, written for this, has no callers (see A3). (b) `SDL_GetVersion`
is never called anywhere in the crate, so the two device assumptions that are *not* pinned — the
shifted `SDL_KeyboardEvent` offsets and the 1080p plane — have no boot-time assertion; one log line
would turn "the remote does nothing" into a one-line diagnosis. **low** → **Wave 3**

Not carried forward: the critic's claim that `session.rs`/PIN logging is a credential leak (see
8.b), and the claim that `SDL_VIDEO_ALLOW_SCREENSAVER=0` (`app.rs:252`, process-wide, never
re-enabled — verified) constitutes a defect. The hint is real and an idle policy is a reasonable
product ask, but the "60 fps forever with the panel lit" measurement was taken on the TV and I did
not re-run it, so it is **unverified** here.

### 8.d Revised coverage note — replaces §7's last bullet

**What is now verified.** Everything in §8.a and §8.c I re-opened, and where a claim was
mechanical I re-ran it: the host `cargo check`/`cargo test` sequence (A1, including the GL link
boundary and two passing tests), the `cmap` parse of both bundled fonts (C10), the tree-wide greps
behind C1, C2, C4 and C14, and the `image` crate's `Limits::default` in the vendored source (A15).
The 42 orphaned findings are closed: 20 confirmed, 22 refuted.

**§7 understated the hole, and the coverage critic was right about its shape.** §7's "not audited"
list named six items, only one of which (`src/gpdebug.c`, 69 lines, unreferenced) is shipped
Rust/C. Measured: of **21,415 lines of shipped Rust + C**, the original report cited **25 files /
4,223 lines** not at all. This addendum adds first citations for `src/main.c`, `img.rs`,
`remote.rs`, `ui/consts.rs`, `plex/transcoder.rs`, `ui/profiles.rs` and `ui/info_panel.rs`, leaving
**~2,340 lines across 15 files with zero citations in either pass**:

`ui/library.rs` (994) · `ui/press.rs` (213) · `ui/theme.rs` (206) · `net.rs` (146) ·
`ui/account_menu.rs` (135) · `plex/library.rs` (117) · `plex/params.rs` (88) · `ui/profile.rs` (85) ·
`system.rs` (81) · `ui/label.rs` (80) · `plex/timeline.rs` (58) · `src/svg.c` (54) ·
`plex/hubs.rs` (30) · `player/ffi.rs` (29) · `svg.rs` (25)

`ui/library.rs` is the material gap: 994 lines, the largest single screen, landed recently, and the
only screen with its own paged store, A-Z rail and server-driven menus. `net.rs` (the only TLS path
in the app) and `system.rs` (the Wayland transparency seam, a documented device-verified invariant)
are the next two worth a dedicated pass. Also still unread: all five `stub/*.c`, `tools/*.sh`,
`tools/stream-screen.py`, `tests/run.py` internals, `vendor/nanosvg`, and the GLSL beyond the dead
`u_focus` branch.

**Nothing in this addendum was run on the TV.** Two claims lean on device state I did not
re-observe and are marked `unverified` in place: the contents of the TV's `/usr/share/fonts` (C10)
and the idle frame-rate/panel behaviour (end of §8.c).
