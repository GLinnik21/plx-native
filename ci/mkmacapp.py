#!/usr/bin/env python3
"""Build `PlxNative.app` — the app core as a SELF-CONTAINED macOS application.

This is the `hostsim` build (`rust-modules/src/bin/sim.rs`): the same UI, the same Plex data layer
and the same event loop the television runs, wrapped in a bundle that carries its own SDL, its own
fonts and its own copy of every non-system library it links, so it starts on a Mac that has none of
this repo's build environment installed. What it is NOT is a television — there is no video
pipeline off-device (`player/ffi_host.rs`), so Play lands on the app's real failure read-out.
`docs/macos-app.md` is the full account of what is and is not in it.

Three things here are load-bearing, and each is a way this ships broken while working perfectly on
the machine that built it:

  1. **Every non-system dylib is copied in and its recorded path REWRITTEN.** Homebrew records
     absolute install paths (`/opt/homebrew/opt/sdl2_ttf/lib/…`) which resolve on this Mac and on
     no other. The walk is transitive because the second level is where it bites: SDL2_ttf pulls
     freetype, freetype pulls libpng and brotli.
  2. **sdl2-compat loads SDL3 with `dlopen`, not through a load command.** `otool -L` on its
     libSDL2 names no SDL3 at all, so a dependency walk cannot see it; the first name it tries is
     `@loader_path/libSDL3.dylib`. Miss this and the bundle dies at `SDL_Init` on every Mac without
     Homebrew — i.e. on every Mac you would send it to and none you would test it on.
  3. **The bundle is ad-hoc CODESIGNED last.** Apple Silicon refuses to execute unsigned code, and
     every `install_name_tool` write invalidates an existing signature.

Why Python and not a shell script: macOS ships bash 3.2 (no associative arrays), and this is a
graph walk with a verification pass. Same reasoning as `ci/mkipk.py`.

Usage:  ci/mkmacapp.py [--out DIR] [--zip] [--target-dir DIR]
"""
import argparse
import os
import platform
import plistlib
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent

# A dependency under these prefixes is the OS's own: present on every Mac, in the dyld shared
# cache, and not ours to copy (copying one is how you ship a library older than the kernel).
SYSTEM_PREFIXES = ("/usr/lib/", "/System/")
# Paths that are already bundle-relative — either we rewrote them, or the library was built that
# way. Nothing to do, but they must not be mistaken for a leak in the verification pass.
RELATIVE_PREFIXES = ("@rpath/", "@loader_path/", "@executable_path/")


def run(*cmd, **kw):
    return subprocess.run([str(c) for c in cmd], check=True, text=True, **kw)


def deps(macho: Path) -> list[str]:
    """The install names `macho` records, excluding its own id line."""
    out = subprocess.run(["otool", "-L", str(macho)], check=True, text=True,
                         capture_output=True).stdout.splitlines()[1:]
    names = [ln.split(" (compatibility")[0].strip() for ln in out if ln.strip()]
    # A dylib's first entry is its own id; the executable has no such line. Compare on the leaf so
    # this holds both before and after the id rewrite.
    return [n for n in names if Path(n).name != macho.name or not n.startswith("@rpath/")]


def version() -> str:
    """THE version this bundle REPORTS, which is the one its binary reports.

    The number comes from the file that owns it — `rust-modules/Cargo.toml`, which
    `ci/check-package.py` keeps in step with `pkg/appinfo.json` and the control file — but the
    string is `rust-modules/build.rs`'s: exactly `X.Y.Z` for a release build, and the next patch
    plus `-dev` for every other one.

    **The same rule, spelled twice, because this script cannot ask cargo what it emitted.** It has
    to match, and the way it can go wrong is worth stating: the cargo subprocess below inherits
    this process's environment, so it reads the same `PLX_RELEASE` — a Makefile-exported empty
    value under `make macapp`, `1` under `make RELEASE=1 macapp`. Reading `Cargo.toml` alone (which
    this did) put `0.5.0` in the Finder metadata and the zip filename of a bundle whose diagnostics
    panel says `0.5.1-dev`: the very ambiguity the suffix exists to remove, recreated on the one
    artifact that is handed to somebody who has none of this checkout.

    This is the string for the ZIP NAME and the display metadata. The two numeric plist keys take
    `package_version()` below instead — see `write_plist`.
    """
    pkg = package_version()
    if os.environ.get("PLX_RELEASE"):
        return pkg
    major, minor, patch = (int(p) for p in pkg.split("."))
    return f"{major}.{minor}.{patch + 1}-dev"


def package_version() -> str:
    """The version this bundle was CUT FROM — three integers, straight out of the manifest.

    It is what the two Info.plist version keys take, because those are a FORMAT CONTRACT: Apple
    specifies `CFBundleShortVersionString` and `CFBundleVersion` as period-separated integers, and
    that does not relax for an ad-hoc-signed bundle — LaunchServices and anything else that
    compares or displays a bundle version is entitled to assume the shape and to do what it likes
    with `0.5.1-dev`.

    **Not `version()` with the suffix stripped**, which is the obvious spelling and is wrong: that
    yields `0.5.1`, a version no release has ever carried, so the numeric field would claim a
    release that does not exist — strictly worse than naming the one it was built from. The dev
    distinction goes where it costs nothing instead: the archive filename, `CFBundleGetInfoString`
    (free-form by definition), and the app's own About page and diagnostics panel, which is where
    somebody reporting a bug is asked to read it from anyway.
    """
    m = re.search(r'^version\s*=\s*"([^"]+)"',
                  (REPO / "rust-modules/Cargo.toml").read_text(), re.M)
    if not m:
        sys.exit("mkmacapp: no version in rust-modules/Cargo.toml")
    pkg = m.group(1)
    parts = pkg.split(".")
    if len(parts) != 3 or not all(p.isdigit() for p in parts):
        sys.exit(f"mkmacapp: Cargo.toml version {pkg!r} is not three integers")
    return pkg


def build_binary(tdir: Path) -> Path:
    """Cargo, release, with BOTH dev features off.

    `--no-default-features` drops `devtools` (the on-screen counter) and `devtriggers` (the whole
    `/tmp/plxnative-*` surface, the remote FIFO and the capture listener). A binary somebody was
    sent must not take instructions from a world-writable directory — and with the feature off
    there is nothing to take: `dev::flag` is `false` and `dev::read` is `None` at COMPILE time.
    """
    print("==> building plxnative-sim (release, no dev features)")
    run("cargo", "build", "--manifest-path", REPO / "rust-modules/Cargo.toml",
        "--target-dir", tdir, "--release", "--no-default-features",
        "--features", "hostsim", "--bin", "plxnative-sim")
    return tdir / "release/plxnative-sim"


def write_icon(res: Path):
    """`.icns` from the 1254px master — macOS wants a 512@2x rung, and upscaling `pkg/icon320.png`
    into it is visible in the Dock."""
    print("==> icon")
    master = REPO / "assets/logo-master.png"
    with tempfile.TemporaryDirectory() as tmp:
        iconset = Path(tmp) / "AppIcon.iconset"
        iconset.mkdir()
        for sz in (16, 32, 64, 128, 256, 512):
            for scale, name in ((1, f"icon_{sz}x{sz}.png"), (2, f"icon_{sz}x{sz}@2x.png")):
                px = sz * scale
                run("sips", "-z", px, px, master, "--out", iconset / name,
                    stdout=subprocess.DEVNULL)
        run("iconutil", "-c", "icns", iconset, "-o", res / "AppIcon.icns")


def write_plist(contents: Path, ver: str):
    """`NSHighResolutionCapable` is the half that makes `app.rs`'s ALLOW_HIGHDPI window real: with
    it the 960x540-point window gets a 1920x1080 drawable, i.e. `surface::scale() == 1.0` — the
    same 1:1 texel contract the television renders under. Without it the OS hands back a
    half-resolution surface and doubles the whole interface."""
    plist = {
        "CFBundleName": "PlxNative",
        "CFBundleDisplayName": "PlxNative",
        "CFBundleExecutable": "PlxNative",
        "CFBundleIdentifier": "com.beb.plxnative",
        "CFBundleIconFile": "AppIcon",
        "CFBundleInfoDictionaryVersion": "6.0",
        "CFBundlePackageType": "APPL",
        # Numeric by contract (see `package_version`), so these two can read 0.5.0 while the
        # binary beside them reports 0.5.1-dev. That gap is why the free-form key below exists.
        "CFBundleShortVersionString": package_version(),
        "CFBundleVersion": package_version(),
        # Free-form by definition, and the one plist field that may carry the suffix. It is what
        # Finder's Get Info shows, so "which build is this?" is answerable without launching it.
        "CFBundleGetInfoString": f"PlxNative {ver}",
        "LSMinimumSystemVersion": "11.0",
        "NSHighResolutionCapable": True,
        "NSHumanReadableCopyright":
            "© 2026 Gleb Linnik. MIT. Not affiliated with Plex GmbH or LG Electronics.",
        # macOS 15+ asks before letting an app reach the LAN, and a Plex client that cannot is
        # useless. The string is what the person sees in that prompt, so it says why.
        "NSLocalNetworkUsageDescription":
            "PlxNative streams from a Plex Media Server on your local network.",
    }
    (contents / "Info.plist").write_bytes(plistlib.dumps(plist))


def bundle_libraries(binary: Path, frameworks: Path):
    """Copy every non-system dependency in, transitively, and repoint each reference at `@rpath`."""
    print("==> bundling libraries")
    queue = [binary]
    while queue:
        obj = queue.pop(0)
        for dep in deps(obj):
            if dep.startswith(RELATIVE_PREFIXES):
                leaf = Path(dep).name
                if not (frameworks / leaf).exists():
                    print(f"    ! unresolved {dep} in {obj.name}", file=sys.stderr)
                continue
            if dep.startswith(SYSTEM_PREFIXES):
                continue
            leaf = Path(dep).name
            dest = frameworks / leaf
            if not dest.exists():
                shutil.copy2(dep, dest)
                dest.chmod(0o644)
                run("install_name_tool", "-id", f"@rpath/{leaf}", dest)
                queue.append(dest)
                print(f"    + {leaf}  ({dep})")
            run("install_name_tool", "-change", dep, f"@rpath/{leaf}", obj)


def bundle_dlopened_sdl3(frameworks: Path):
    """SDL3, which no load command mentions — see the module doc, point 2.

    Copied under the exact leaf sdl2-compat asks for first (`@loader_path/libSDL3.dylib`, i.e.
    beside libSDL2 in `Frameworks/`), and only when the SDL2 we bundled is in fact sdl2-compat.
    """
    sdl2 = frameworks / "libSDL2-2.0.0.dylib"
    if not sdl2.exists():
        return
    blob = sdl2.read_bytes()
    if b"libSDL3.dylib" not in blob:
        return  # a real SDL2: it needs nothing further
    for cand in ("/opt/homebrew/opt/sdl3/lib/libSDL3.0.dylib",
                 "/usr/local/opt/sdl3/lib/libSDL3.0.dylib",
                 "/opt/homebrew/lib/libSDL3.0.dylib"):
        if Path(cand).exists():
            dest = frameworks / "libSDL3.dylib"
            shutil.copy2(cand, dest)
            dest.chmod(0o644)
            run("install_name_tool", "-id", "@rpath/libSDL3.dylib", dest)
            print(f"    + libSDL3.dylib  ({cand}, dlopen'd by sdl2-compat)")
            bundle_libraries(dest, frameworks)
            return
    sys.exit("mkmacapp: sdl2-compat needs libSDL3 and none was found — the app would not start")


def verify_self_contained(macos: Path, frameworks: Path):
    """The one check that would have caught any of the three traps in the module doc."""
    print("==> verifying self-containment")
    leaks = []
    for obj in sorted(list(macos.iterdir()) + list(frameworks.iterdir())):
        if obj.is_dir():
            continue
        for dep in deps(obj):
            if dep.startswith(RELATIVE_PREFIXES) or dep.startswith(SYSTEM_PREFIXES):
                continue
            leaks.append(f"  {obj.name} -> {dep}")
    if leaks:
        sys.exit("mkmacapp: bundle is NOT self-contained:\n" + "\n".join(leaks))
    print(f"    {len(list(frameworks.iterdir()))} bundled libraries, no external paths")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", default=str(REPO / "pkg"))
    ap.add_argument("--target-dir", default=os.environ.get(
        "MACAPP_TDIR", str(REPO / "rust-modules/target-macapp")))
    ap.add_argument("--zip", action="store_true")
    args = ap.parse_args()

    if platform.system() != "Darwin":
        sys.exit("mkmacapp: macOS only")

    ver = version()
    out = Path(args.out)
    app = out / "PlxNative.app"
    contents, = (app / "Contents",)
    macos, frameworks, res = contents / "MacOS", contents / "Frameworks", contents / "Resources"

    binary = build_binary(Path(args.target_dir))

    print(f"==> assembling {app}")
    if app.exists():
        shutil.rmtree(app)
    for d in (macos, frameworks, res):
        d.mkdir(parents=True)
    shutil.copy2(binary, macos / "PlxNative")
    (macos / "PlxNative").chmod(0o755)

    # The payload the app reads at RUNTIME is fonts and nothing else — every icon is an SVG
    # compiled into the binary (`ui/icons.rs`'s `include_str!`), and `paths::app_dir` resolves
    # `Contents/Resources` when it finds itself inside a bundle. The rest is provenance: the
    # licences a redistributed binary owes.
    for f in ("appfont.ttf", "appfont-bold.ttf", "OFL.txt"):
        shutil.copy2(REPO / "pkg" / f, res / f)
    shutil.copy2(REPO / "LICENSE", res / "LICENSE.txt")
    shutil.copy2(REPO / "THIRD-PARTY-NOTICES.md", res / "THIRD-PARTY-NOTICES.md")

    write_icon(res)
    write_plist(contents, ver)

    bundle_libraries(macos / "PlxNative", frameworks)
    bundle_dlopened_sdl3(frameworks)
    # One rpath, on the executable: every rewritten reference is `@rpath/<leaf>` and resolves
    # through it, including the ones inside the bundled libraries themselves.
    run("install_name_tool", "-add_rpath", "@executable_path/../Frameworks", macos / "PlxNative")
    verify_self_contained(macos, frameworks)

    # Ad-hoc signing is what is available without a paid Developer ID: it satisfies Apple Silicon's
    # "code must be signed to run at all" rule, and does NOT satisfy Gatekeeper's "signed by a known
    # developer" rule — so the person receiving this allows it once, by hand. The README says how.
    print("==> signing (ad-hoc)")
    run("codesign", "--force", "--deep", "--sign", "-", "--timestamp=none", app,
        stderr=subprocess.DEVNULL)
    run("codesign", "--verify", "--deep", "--strict", app, stderr=subprocess.DEVNULL)
    print("    signature ok")

    readme = REPO / "docs/macos-app-readme.md"
    if readme.exists():
        shutil.copy2(readme, out / "PlxNative-README.md")

    if args.zip:
        zip_path = out / f"PlxNative-{ver}-{platform.machine()}.zip"
        zip_path.unlink(missing_ok=True)
        # ditto, not zipfile: it is the tool that preserves the extended attributes and symlink
        # layout a signed bundle needs. A plain zip can arrive with a broken signature.
        run("ditto", "-c", "-k", "--sequesterRsrc", "--keepParent", app, zip_path)
        print(f"==> {zip_path}  ({zip_path.stat().st_size // (1024*1024)} MB)")

    total = sum(f.stat().st_size for f in app.rglob("*") if f.is_file())
    print(f"==> {app}  ({total // (1024*1024)} MB, arch {platform.machine()}, version {ver})")


if __name__ == "__main__":
    main()
