//! plx_gfx: what the PlxNative application core draws with.
//!
//! The GLES2 renderer (`gfx`: the shader programs, the draw primitives, the live-backdrop walk,
//! the design tokens and the draw-phase instruments), the boot-time EGL probe (`egl`), text
//! rasterization over SDL2_ttf (`text`), image decoding (`img`), the runtime SVG rasterizer's FFI
//! (`svg`, which calls into `src/svg.c`), and the GPU instruments: the async timer queries
//! (`gpu_timer`), the Mali counter reader (`hwcnt`) and the per-draw-class overdraw ledger
//! (`overdraw`, which lived in `ui` until the split). It is the `gfx` layer of
//! `ci/module-layers.ini`: it uses `base` and `machine` (and may use `platform`, which nothing
//! here names today).
//!
//! The crate declares the `extern "C"` blocks (GLES2, EGL, SDL2_ttf, nanosvg) but links nothing on
//! the television: the final link stays the Makefile's. On the host, the application crate's build
//! script supplies SDL, GL and the nanosvg object for the application's own binaries, and this
//! crate's build script does the same for this crate's own test binary, which is a real
//! executable that reaches those symbols (`build_support/host_link.rs`, shared by both).
//!
//! `test-support` exposes the `cfg(test)` seams other layers' tests build on (`text`'s prewarm and
//! capture hooks, `gfx::backdrop::commit`, `gfx::GLASS_REGION_BUDGET`, `gfx::FieldTicket::for_test`)
//! and keeps this crate's test-mode behaviour when a dependent's tests build it. The application
//! crate enables it in `[dev-dependencies]` only, so no shipped build sees it.

pub mod dump; // dump mode's gfx half: prewarm drains to empty, GPU readiness forced (default off)
pub mod egl; // boot-time EGL capability probe (extensions, swap behaviour, buffer age) — diagnostic only
pub mod gfx;
#[cfg(feature = "devtriggers")]
pub mod gpu_timer; // async EXT_disjoint_timer_query timing; no glFinish on the timing path
#[cfg(feature = "devtriggers")]
pub mod hwcnt; // direct userspace Mali r12p0 vinstr reader for the phase profiler
pub mod img;
pub mod overdraw; // dev-only DRAW-CLASS ledger + mask — the attribution instrument (docs/backdrop-blur-profiling.md Part 5)
pub mod svg; // runtime SVG rasterizer FFI (src/svg.c / nanosvg) — vector icon assets
pub mod text;
