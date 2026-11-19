//! Link the STATIC SDL2 + SDL2_ttf that `sdl/build-sdl.sh` built, when `build.sh` (or you) point
//! `PLX_SDL_PREFIX` at its product (`tizen/hello/sdl-build/<arch>/`).
//!
//! * Set: the two archives are linked BY ABSOLUTE PATH, in dependency order (SDL2_ttf, then SDL2), and
//!   then the system libraries SDL needs (`-lm ...`, from `link-libs.txt`), all as link arguments so they
//!   land after the Rust objects. Not `-lSDL2`: the NDK sysroot's own `libSDL2.a` is on the ARM link's
//!   library path and a `-l` lookup could pick it. `--cfg plx_sdl` turns the SDL code on. A prefix that is
//!   set but incomplete is a build error: a `.so` must never silently ship without the probe.
//! * Unset: no SDL is linked and the probe compiles to a stub that logs "built without SDL" (plain
//!   `cargo build` / `cargo test` need no SDL build). The H5 unit test then does not run.
//!
//! `libplx.so` gets no `DT_NEEDED` for SDL either way: the code is inside the object.

use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-env-changed=PLX_SDL_PREFIX");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rustc-check-cfg=cfg(plx_sdl)");

    let Some(prefix) = env::var_os("PLX_SDL_PREFIX").filter(|p| !p.is_empty()) else {
        return;
    };
    let prefix = PathBuf::from(prefix);
    let archives = [prefix.join("lib/libSDL2_ttf.a"), prefix.join("lib/libSDL2.a")];
    for a in &archives {
        assert!(
            a.is_file(),
            "PLX_SDL_PREFIX={} has no {}; run tizen/hello/sdl/build-sdl.sh <arch> first",
            prefix.display(),
            a.display()
        );
        println!("cargo:rerun-if-changed={}", a.display());
    }
    let extra_path = prefix.join("link-libs.txt");
    let extra = fs::read_to_string(&extra_path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", extra_path.display()));
    println!("cargo:rerun-if-changed={}", extra_path.display());
    let pins = fs::read_to_string(prefix.join("pins.txt")).unwrap_or_default();

    println!("cargo:rustc-cfg=plx_sdl");
    println!("cargo:rustc-env=PLX_SDL_PINS={}", pins.trim());
    for a in &archives {
        println!("cargo:rustc-link-arg={}", a.display());
    }
    for token in extra.split_whitespace() {
        println!("cargo:rustc-link-arg={token}");
    }
    // glibc keeps a few functions (`atexit`, and `stat`/`fstat` before glibc 2.33) only in
    // libc_nonshared.a, which the `-lc` linker script pulls in. std's own `-lc` comes BEFORE these
    // archives, so what SDL needs from it (SDL_evdev_kbd.o calls atexit) would stay undefined, and
    // libc.so.6 does not export `atexit`: the library would fail to load on the TV. A second `-lc`
    // after the archives lets the group resolve them. (`build.sh` fails the build on any other
    // unversioned undefined symbol, so a regression is caught.)
    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        println!("cargo:rustc-link-arg=-lc");
    }
}
