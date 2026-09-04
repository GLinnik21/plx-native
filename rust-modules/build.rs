//! Two jobs: the version string every build reports, and link configuration for the host UI
//! simulator.
//!
//! The first runs for EVERY build (see [`emit_version`]). The second is a **no-op for every build
//! that matters**: the television binary is linked by the Makefile, not by cargo (the crate is a
//! staticlib; a staticlib has no link step), so the ARM build reaches the early return below and
//! emits nothing further. Only `--features hostsim`, which builds an actual executable, gets past
//! it.
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

    emit_version();
    emit_build_sha();

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

/// Publish `PLX_VERSION` — the version this build REPORTS, which is not always the version it was
/// cut from.
///
/// `Cargo.toml`, `pkg/appinfo.json` and `ipkroot/ctl/control` all carry the same three integers,
/// and `ci/check-package.py` fails the build if they ever disagree: LG accepts nothing else in a
/// package, so the published number has to stay exactly `X.Y.Z`. But a release commit leaves the
/// tree AT the version it just published, so every developer build after it reported that number
/// as its own — to `X-Plex-Version` on the account's authorized-devices list, to Sentry as the
/// release `plxnative@X.Y.Z`, to PostHog as `app_version`, and on the diagnostics panel that is
/// meant to be photographed into a bug report. A crash from somebody's working tree landed on the
/// shipped release's tally and nothing downstream could separate the two.
///
/// So a build that is not a release names the version it is working TOWARDS and says what it is:
/// `0.5.0` published, `0.6.0-dev` in the tree.
///
/// **It is the next MINOR, because this project is trunk-based.** Trunk is where features land, so
/// the next thing cut from it is a minor (or a major, when the maintainer decides that); a PATCH is
/// only ever cut from an existing minor's own line, where trunk's number is not the question being
/// asked. Naming the next patch would therefore be wrong in the common case and right in none —
/// `0.5.1-dev` on trunk claims to be heading for a release that, by policy, is not cut from trunk
/// at all. It also makes the semver ordering mean something: `0.6.0-dev` precedes `0.6.0`, so the
/// tree really is a pre-release of what it is heading for, rather than of a version nobody plans.
///
/// A MAJOR is not guessed at, and cannot be: nothing in this file can see that decision, and
/// `1.0.0-dev` claimed on every ordinary build would be a false promise rather than a cautious one.
/// The invariant that actually matters is weaker and is preserved either way — the reported version
/// must never be a number a release has already used.
///
/// If a maintenance line is ever cut (a `0.5.x` branch for a patch release), the rule here needs an
/// input it does not have today, because on that branch the next version IS a patch. Nothing in the
/// repo cuts one yet; the fix at that point is an env override beside `PLX_RELEASE`, not a git
/// branch lookup — a build script that reads git state is wrong in a tarball, in CI and in a
/// worktree.
///
/// **The suffix reaches the reported string ONLY.** It never enters `pkg/appinfo.json` or the
/// control file: `1.0.0-rc1` is not installable on a webOS television, which is also why
/// `ci/bump-version.py` refuses to write anything but three integers. `ci/check-package.py`
/// gates the other direction — a package built for the stable id may not carry a `-dev` binary.
///
/// The input is `PLX_RELEASE`, exported by the Makefile for `RELEASE=1` and by nothing else, so
/// the developer answer is what an ordinary `make`, `make check`, `make sim` or a bare `cargo
/// build` produces. `rerun-if-env-changed` makes cargo re-run this when that flips; the emitted
/// value is itself tracked, so the crate rebuilds with it.
fn emit_version() {
    println!("cargo:rerun-if-env-changed=PLX_RELEASE");
    let pkg = std::env::var("CARGO_PKG_VERSION").expect("CARGO_PKG_VERSION");
    // PARSED BEFORE THE BRANCH, deliberately. Validating only inside the developer arm would make
    // the shape rule conditional on the build that is least likely to be looked at: cargo accepts
    // `0.5.0-rc.1` in a manifest, LG accepts it in no package at all, and a release build is
    // exactly where that must not compile quietly.
    // The patch component is validated and then DISCARDED — see below.
    let (major, minor, _patch) = triplet(&pkg);
    // Set-but-empty is not "release": the Makefile exports the variable unconditionally and
    // leaves it blank for a dev build, the same shape `telemetry::sender` reads its credentials
    // with.
    let release = std::env::var("PLX_RELEASE").is_ok_and(|v| !v.is_empty());
    let version = if release {
        pkg
    } else {
        // Discarded rather than incremented: the next thing cut from trunk is a minor, and
        // `0.5.3` + a minor is `0.6.0`, not `0.6.3`.
        //
        // `u64` and `checked_add` so the three implementations of this arithmetic agree on every
        // input rather than on realistic ones: cargo's own semver allows components up to
        // `u64::MAX`, and python's integers are unbounded, so a narrower type here would be the
        // one of the three that answers differently. Unreachable for any version anyone writes,
        // which is exactly why it should not be a silent wrap.
        let next = minor
            .checked_add(1)
            .unwrap_or_else(|| panic!("Cargo.toml version {pkg:?} has no next minor"));
        format!("{major}.{next}.0-dev")
    };
    println!("cargo:rustc-env=PLX_VERSION={version}");
}

/// The manifest version as three integers, or a build failure.
///
/// A version that is not three integers is a hard error rather than a passthrough: every gate in
/// `ci/` asserts that shape, so reaching here with anything else means the manifest was edited by
/// hand into a state that cannot be packaged, and failing at the build is where that costs least.
fn triplet(pkg: &str) -> (u64, u64, u64) {
    let parts: Vec<&str> = pkg.split('.').collect();
    // Split first, parse second, and drop NOTHING in between: a `filter_map(parse)` reads as the
    // same thing and is not — `0.5.0-rc.1` splits into four parts, one of which does not parse,
    // and silently becomes (0, 5, 1). The shape has to fail, not degrade.
    let [major, minor, patch] = parts[..] else {
        panic!("Cargo.toml version {pkg:?} is not three dot-separated integers");
    };
    let int = |part: &str| {
        part.parse()
            .unwrap_or_else(|_| panic!("Cargo.toml version {pkg:?} is not three integers"))
    };
    (int(major), int(minor), int(patch))
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
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("rust-modules has a parent");
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
        .unwrap_or_else(|e| {
            panic!(
                "hostsim: could not run {cc:?} to compile {}: {e}",
                src.display()
            )
        });
    assert!(
        st.success(),
        "hostsim: compiling {} failed ({st})",
        src.display()
    );
    // Link the object directly; no intermediate archive, so no `ar` involved.
    println!("cargo:rustc-link-arg-bins={}", out.display());
}

/// Publish `PLX_BUILD_SHA` — the short commit this binary was built from, so the About screen can
/// name the exact source a bug report came from. `PLX_VERSION` alone cannot: every commit on trunk
/// between two releases reports the identical `X.Y.0-dev`.
///
/// Best-effort and NOT [`emit_version`]'s contract: a release source package (`docs/distribution.md`
/// publishes one per release) has no `.git` at all and must still build, so a failed `git` falls
/// back to `"unknown"` rather than failing the build the way an unparsable `Cargo.toml` version
/// does. Re-run on whatever moves this WORKTREE's `HEAD` — a commit, checkout or rebase all touch
/// its reflog — via `git rev-parse --git-path`, which resolves `logs/HEAD`/`HEAD` under the linked
/// worktree's own `.git/worktrees/<name>/`, not the main checkout's; watching the wrong one would
/// silently miss every commit made from here.
fn emit_build_sha() {
    let sha = git_short_sha().unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=PLX_BUILD_SHA={sha}");
    for rel in ["logs/HEAD", "HEAD"] {
        if let Some(path) = git_path(rel) {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
}

fn git_short_sha() -> Option<String> {
    let out = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let sha = String::from_utf8(out.stdout).ok()?;
    let sha = sha.trim();
    (!sha.is_empty()).then(|| sha.to_string())
}

/// The absolute path `git` resolves `<rel>` (a path inside `.git`) to for the CURRENT worktree.
fn git_path(rel: &str) -> Option<PathBuf> {
    let out = Command::new("git")
        .args(["rev-parse", "--path-format=absolute", "--git-path", rel])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let p = String::from_utf8(out.stdout).ok()?;
    Some(PathBuf::from(p.trim()))
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
