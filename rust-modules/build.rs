//! Link configuration for the host UI simulator — and nothing else.
//!
//! This script is a **no-op for every build that matters**. The television binary is linked by the
//! Makefile, not by cargo (the crate is a staticlib; a staticlib has no link step), so the ARM
//! build reaches the early return below and emits nothing. Only `--features hostsim`, which builds
//! an actual executable, gets here.
//!
//! Why a build script rather than `.cargo/config.toml`, which is where this crate's other
//! target-bound flags live: the library search path is not a constant. Homebrew is at
//! `/opt/homebrew` on Apple Silicon and `/usr/local` on Intel, and a hardcoded guess fails as an
//! "SDL2 not found" link error a long way from its cause. Asking `brew` is the only answer that is
//! right on both.

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    // The simulator is the only configuration that links anything here. Checked via the feature's
    // env var rather than `cfg!`, because a build script is compiled for the HOST and its own
    // `cfg!(feature = ...)` would answer for the script, not for the crate being built.
    if std::env::var_os("CARGO_FEATURE_HOSTSIM").is_none() {
        return;
    }

    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();

    if let Some(libdir) = sdl_search_path() {
        println!("cargo:rustc-link-search=native={}", libdir.display());
    } else {
        // Not fatal — a system-wide install needs no search path. Say so anyway, because the
        // alternative is an undefined-symbol wall with no explanation.
        println!(
            "cargo:warning=hostsim: no Homebrew lib dir found; relying on the default linker \
             search path for SDL2/SDL2_ttf"
        );
    }

    println!("cargo:rustc-link-lib=dylib=SDL2");
    println!("cargo:rustc-link-lib=dylib=SDL2_ttf");

    match target_os.as_str() {
        "macos" => {
            // GL entry points come from the framework. The simulator asks for a 4.1 core context;
            // see `app.rs`'s context-attribute branch for why it cannot ask for GLES2 here.
            println!("cargo:rustc-link-lib=framework=OpenGL");
        }
        _ => {
            println!("cargo:rustc-link-lib=dylib=GL");
        }
    }

    compile_svg();
}

/// Build `src/svg.c` (the nanosvg rasterizer) for the host.
///
/// On the television the Makefile compiles this alongside `main.c` and `starfish.c` and links all
/// three with the staticlib. The simulator has no Makefile step, so it does the one piece it still
/// needs — `svg.c` is portable C with no webOS in it, and the icon set is rasterized from SVG at
/// runtime, so without it the UI links but has no icons.
///
/// Shelling out to the C compiler rather than taking the `cc` crate as a build-dependency: this is
/// one translation unit with two include paths, and the crate's dependency list is deliberately
/// short — a build-dep would be fetched for the ARM build too, which never runs this function.
fn compile_svg() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("rust-modules has a parent");
    let src = repo.join("src/svg.c");
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR")).join("svg.o");
    println!("cargo:rerun-if-changed={}", src.display());

    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".into());
    let st = Command::new(&cc)
        .arg("-c")
        .arg(&src)
        .arg("-o")
        .arg(&out)
        .arg("-O2")
        .arg("-fPIC")
        // Matches the Makefile's include set for this file.
        .arg(format!("-I{}", repo.join("src").display()))
        .arg(format!("-I{}", repo.join("vendor/nanosvg").display()))
        .status()
        .unwrap_or_else(|e| panic!("hostsim: could not run {cc:?} to compile {}: {e}", src.display()));
    assert!(st.success(), "hostsim: compiling {} failed ({st})", src.display());
    // Link the object directly; no intermediate archive, so no `ar` involved.
    println!("cargo:rustc-link-arg-bins={}", out.display());
}

/// Homebrew's lib directory, asked of `brew` itself and sanity-checked, or `None`.
fn sdl_search_path() -> Option<PathBuf> {
    let out = Command::new("brew").arg("--prefix").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let prefix = String::from_utf8(out.stdout).ok()?;
    let libdir = Path::new(prefix.trim()).join("lib");
    libdir.is_dir().then_some(libdir)
}
