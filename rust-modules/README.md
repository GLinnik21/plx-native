# rust-modules — the Rust half of the hybrid app

This crate is a `staticlib` linked into the C binary (`pkg/plxnative`). It's the
result of a gradual, module-by-module C→Rust migration on the `rust-migration`
branch. Each ported module exposes the **same C ABI** its `src/*.h` declares, so
the remaining C code calls it unchanged; the corresponding `src/*.c` is excluded
from the C build via the Makefile's `filter-out`.

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

## Left as C by design (2) — the Starfish C++ pipeline + its event loop

- **`src/playback.c`** — LG's `StarfishMediaAPIs` (11 mangled C++ methods called
  via `__asm__`, an `sret` `std::string` return, a 64 KB in-place object buffer)
  plus the ACB video-plane bind and the buffer-feed pump.
- **`src/main.c`** — the SDL event loop tightly coupled to that pump.

Rust *can* do this FFI (`#[link_name = "<mangled>"]` mirrors the C `__asm__`), but
it would be **all `unsafe` with zero safety gain**, high effort, and high risk to
the hardest-won feature (video on the panel). Per the project's "drop Rust if it
develops worse than C" guardrail, C stays the right tool for the C++ interop.

## Build (see the Makefile for the full toolchain)

Cross-compiled with `cargo +nightly zigbuild` for `arm-unknown-linux-gnueabi.2.24`.
Critical TV flags: `-C target-cpu=cortex-a53` (the default ARMv6 codegen emits a
CP15 memory barrier that's UNDEFINED on this A53 → SIGILL), `-C target-feature=-neon`,
and `-Z build-std` (rebuilds std with those flags). Linked with `-lunwind` for
std's ARM-EHABI unwinder.
