# rust-modules — the Rust app core

This crate is a `staticlib` linked into the C binary (`pkg/plxnative`). It began as a
gradual, module-by-module C→Rust migration; today it IS the app — UI, event loop, player
engine, demux pipeline, and the Plex data layer all live here (see `docs/agent-reference.md`
for the architecture). The C side only boots the process and wraps the Starfish C++ seam;
the crate's C surface is down to `plex_run` + the two starfish callbacks.

## Ported to Rust (10) — the whole data / logic / UI / render / platform stack

| module | what it is | notes |
|--------|-----------|-------|
| `img` | image decode + GL upload | `image` crate replaces vendored stb_image.h |
| `stream` | HTTP/1.1 client | `repr(C)` `http_stream`; bounds-checked parsing |
| `aq` | access-unit FIFO | `repr(C)` + libc pthread; flexible-array node |
| `mkv` | Matroska/EBML demuxer | ~450 lines; `catch_unwind` entry points |
| `pms` | Plex catalog fetch/parse | `serde_json` replaces the hand-scrape; shared `pms_movies[]` |
| `posters` | async artwork store | idiomatic rewrite on `std::sync` Mutex/Condvar + threads |
| `text` | SDL2_ttf rendering | font + glyph-texture caches |
| `gfx` | GLES2 draw primitives | 3 shader programs, SDF cards, FPS digits |
| `system` | SDL/wayland platform glue | the over-allocated `SDL_SysWMinfo` buffer lives here |
| `ui_home` | home screen + navigation | reusable component seed (card/pill/circle/label) |

## Left as C by design (3)

- **`src/starfish.c`** — LG's `StarfishMediaAPIs` (the mangled C++ methods called
  via `__asm__`, an `sret` `std::string` return, a 64 KB in-place object buffer)
  plus the ACB video-plane bind. The engine that *drives* it is Rust (`player/`).
- **`src/main.c`** — the boot shim: crash tracer, event-log/stderr setup, process
  bring-up; it calls the Rust `plex_run()` and owns nothing else.
- **`src/svg.c`** — the vendored nanosvg rasterizer (header-only C).

Rust *can* do the Starfish FFI (`#[link_name = "<mangled>"]` mirrors the C
`__asm__`), but it would be **all `unsafe` with zero safety gain**, high effort,
and high risk to the hardest-won feature (video on the panel). C stays the right
tool for the C++ interop.

## Build (see the Makefile for the full toolchain)

Cross-compiled with plain `cargo +nightly build -Z build-std=std,panic_unwind` for
`arm-unknown-linux-gnueabi`; the C side and the final link use the webOS NDK
(`make setup-env`). Critical TV flags: `-C target-cpu=cortex-a9` (the default
ARMv6 codegen emits a CP15 memory barrier that SIGILLs on this SoC; cortex-a9
emits the dedicated `dmb`) and `-C target-feature=-neon`, with `-Z build-std`
rebuilding std under those flags. The Makefile owns this invocation — nothing
else should duplicate it.
