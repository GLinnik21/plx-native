# Rust-first migration plan

Target: a **Rust-first app with C only for low-level subsystems**. Today it's the
inverse — a C core (`main.c`) with Rust leaf modules. Synthesized from a 3-architect
design workflow (incremental-safety / target-structure / build-toolchain).

## The keystone decision (why this is low-risk)

Keep the final link as the **existing byte-identical `zig cc` invocation** and keep the
Rust device crate a **`staticlib`**. Invert ownership by having a **thin C `main()` call
a Rust `plex_run()`** — *not* by making cargo own the link, and *not* by defining `main`
in Rust. This preserves the stub-SONAME `DT_NEEDED` trick, `-Z build-std`, the
CP15/`-neon` flags, and `-lunwind` placement untouched, so the video pipeline stays
bit-identical while entry ownership flips. A real C `main` also means the crash tracer /
log / `freopen(stderr)` / `setenv` run *before* any Rust — a Rust panic in `plex_run` is
still traced.

## End state

- **C (two files):**
  - `src/starfish.c` + `.h` — the StarfishMediaAPIs C++ ABI (11 mangled `__asm__` symbols,
    the `sret std::string` in Feed, the 64 KB in-place `g_smp` object) + ACB binding (the
    3-arg `taskId` ABI), behind flat `sf_*` / `acb_*` verbs. Two `sf_on_event`/`acb_on_event`
    edges call back into panic-guarded Rust.
  - `src/main.c` — a ~40-line boot shim (log, `freopen` stderr, install the async-signal-safe
    crash tracer, `setenv`, then `return plex_run()`).
- **Rust:** everything else — `plex_run` + the SDL event loop, input decode, tick, draw
  orchestration, lifecycle, dev triggers, `play_movie` routing, the HUD, and the whole
  buffer-feed engine (pump + demux/cue/load threads + seek/rebase) driving the C seam — plus
  the 10 already-ported modules. (Optional end-state: split the portable logic into a
  host-testable `core/` crate.)

## Migration order — each step builds, deploys, and is video-verified on the TV

1. **Isolate the C++/ACB seam** into `src/starfish.c` behind `sf_*`/`acb_*` verbs (pure-C
   refactor, zero behavior change; `playback.c` calls the seam). *Verify:* movie plays, full
   bind sequence in the log, seek/pause/bg-fg identical. **← safe first move.**
2. **Invert the entry point:** `main.c` body → Rust `plex_run()`; `main.c` → the boot shim.
   Port the LG raw-key decode (over-allocated event buffer + `read_unaligned` at +16/+20/+24).
   *Verify:* full remote smoke; `nm` shows `plex_run` in the `.a`. **"Rust owns main."**
3. **Port `play_movie` routing** (direct-play vs `/decision`+`start.mkv` transcode) → Rust.
4. **Port `draw_hud`** → `ui::player_hud` (a View over a Hud snapshot). *Verify:* capture-diff.
5. **Port the buffer-feed engine** → Rust (**highest risk**): pump, threads, seek/rebase, cue
   index, the two callbacks (panic-guarded `#[no_mangle]`), cross-thread state → `Arc<Shared>`
   (atomics + Mutex), state flags → one `enum Stage`. **Delete `playback.c`.** *Verify hard:*
   feed Ok, frames climbing, `setMediaVideoData`→PLAYING, video visible, seek, pause/resume,
   bg→fg resume; sourceInfo envelope handed to ACB byte-for-byte.
6. **Internal Rust refactor** to a `Screen` router + Home/Player screens; `fr/fc/snapTarget`
   stop being `#[no_mangle]` and become private `Home` fields. (Detail/Settings become Screens.)
7. **(Optional)** Extract `core/` host-test workspace + `cargo test` against fixtures.

## Guardrails

- **Build:** keep the `zig cc` link byte-identical — no `build.rs`, no cargo-owned link, no
  stub edits, no `appinfo.json` change. The only allowed link delta is adding a *new* SDL/GL
  symbol name to the matching `stub/*.c` (same rule as the C build).
- **Video pipeline:** step 5 is the risk (library-thread callback racing app-thread state).
  The seam-split checkpoint (step 1) proves parity first; every step is independently
  deployable and git-revertible; the event log is the oracle.
- **Dedup while moving:** one Rust `log()`; unify the per-module SDL/GL externs into
  `platform::ffi`; the two Load payloads → one JSON builder; cross-thread volatiles → `Arc<Shared>`;
  `repr(C)` mirror structs revert to idiomatic Rust once the C callers are gone.
