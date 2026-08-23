# `PlxNative.app` — the app as a self-contained macOS application

*Written 2026-08-16, when the bundle was first built and verified.*

`make macapp` (or `make macapp-zip`) produces `pkg/PlxNative.app`: the same application core the
television runs, packaged so it starts on a Mac that has none of this repo's build environment —
no Homebrew, no SDL, no toolchain. `ci/mkmacapp.py` is the whole recipe; this note is why it looks
the way it does and what the result is and is not.

It is **not a new build of the app**. It is `--features hostsim` — the desktop simulator
(`docs/…`/`rust-modules/src/bin/sim.rs`, and the `ui-sim` skill) — with the dev features off and a
bundle around it. Every UI change, every Plex data-layer change, is the same code the ipk ships.

## What it can do, and the one thing it cannot

**Can:** sign in with the real plex.tv QR flow, discover the account's servers, browse Home, the
libraries, Search, detail pages, seasons, episodes, cast, the who's-watching picker, watched state
— against a real Plex Media Server on the LAN. It persists the session and comes back signed in.

**Cannot: play video.** The decode path is LG's in-process `StarfishMediaAPIs` bound to the
hardware video plane via ACB — 29 symbols that exist on a television and nowhere else.
`player/ffi_host.rs` reports the same failure a firmware with no usable video path reports, so
**Play lands on the app's real full-screen failure read-out** rather than hanging. That was already
the simulator's contract; the bundle inherits it. A Mac playback backend would be a new media
engine, not a packaging change.

Browsing is not LAN-only: the same `http.rs` control façade runs here, using `stream.rs` for
plaintext hostname/IPv4/IPv6 origins and `net.rs`/libcurl for HTTPS PMS control. The media path is
also present structurally (`curlio.rs` for HTTPS), but playback still ends at the host FFI seam
described above because macOS has no Starfish/ACB decoder backend.

## What had to change for it to work at all

Four things, and three of them were bugs that had been sitting in the tree unreachable:

1. **libcurl was never openable off-device** (`net.rs`). The SONAME candidate list held
   `libcurl.so.4` and `libcurl.so.5` — both correct for webOS, both meaningless on macOS — so
   `net::global_init` reported "no libcurl" and the entire plex.tv half of the app was dead on the
   desktop. This had been written up as a property of the simulator ("QR sign-in does not work
   here"); it was one missing candidate. macOS ships libcurl in the dyld shared cache and
   `dlopen("libcurl.4.dylib")` resolves it with nothing installed and nothing bundled.

2. **…and the moment it opened, sign-in SIGSEGV'd inside libcurl.** `curl_easy_setopt` is
   variadic. `dynlib!` bound it, deliberately, as three non-variadic wrappers with the trailing
   argument spelled concretely — which is right about the *types* and wrong about the *convention*:
   **Apple's ARM64 ABI passes variadic arguments on the stack while named ones go in registers.**
   libcurl read the stack, got rubbish, and dereferenced it in `strlen`. `dynlib!` now takes a
   literal `...` in the position `curl.h` puts it and emits a genuinely variadic function type;
   `dynlib_wrapper!` is the per-function arm that makes that possible. ARM32 and x86-64 pass these
   two ways identically, so **no amount of device testing could ever have found this** — and
   equally, the fix cannot change device behaviour.

3. **`--no-default-features` did not build.** `[lints.rust] warnings = "deny"` (added 2026-08-15)
   plus the seven-segment counter's ungated `SEG`/`draw_digit`/`draw_number` meant three
   `dead_code` errors in exactly the configuration nothing routinely builds. That is the
   configuration `make RELEASE=1` and the release CI job use, so this was broken for the ipk too,
   not only for the bundle.

4. **`paths.rs` learned what a macOS bundle is.** An app bundle keeps its executable in
   `Contents/MacOS` and its payload in `Contents/Resources`, so "next to the binary" is wrong by
   one directory — and getting it wrong is the silent font fallback that module's doc opens with.
   A bundled app also gets `~/Library/Application Support/PlxNative` as its runtime root instead of
   `/tmp`, because that root is where `auth.json` lives and `/tmp` is swept: the friend would be
   re-doing the QR sign-in every few days with nothing to explain why. Both are structural probes
   (is there a `Contents/MacOS` above me?), so the dev loop and every `PLXNATIVE_RUNTIME_DIR`
   recipe are untouched.

The window changed too, though that is taste rather than a bug: it opens at an exact integer
divisor of the 1920x1080 canvas (`app.rs`'s `desktop_window_size`) with ALLOW_HIGHDPI, and is not
resizable. On a Retina display that means a 960x540-point window with a **1920x1080 drawable** —
`surface::scale() == 1.0`, the same 1:1 texel contract the television renders under, which is what
keeps the text as crisp as it is on the panel. A non-Retina 1080p display gets 960x540 at scale
0.5 instead; a 1:1 surface there would need real fullscreen, which no key currently toggles.

## Bundling: the three traps

All three are in `ci/mkmacapp.py`'s module doc, because they belong with the code — repeated here
only in outline. Each one works perfectly on the machine that built the bundle and fails on every
other, which is the class of bug worth naming twice:

- Homebrew records **absolute install paths**; every non-system dylib is copied in and rewritten to
  `@rpath`, transitively (SDL2_ttf → freetype → libpng, brotli, …).
- **sdl2-compat loads SDL3 with `dlopen`**, so no dependency walk can see it. It is copied in under
  the exact leaf name sdl2-compat tries first (`@loader_path/libSDL3.dylib`).
- **Ad-hoc codesigning is last**, because every `install_name_tool` write invalidates a signature —
  and Apple Silicon will not execute unsigned code at all.

The script ends by walking every Mach-O in the bundle and failing if any recorded path points
outside it. That check is the only reason to trust the result without a second Mac to test on.

## Distribution

Ad-hoc signing satisfies "must be signed to run" and not "signed by a known developer", so the
recipient does **right-click → Open** once, or clears the quarantine attribute by hand.
`docs/macos-app-readme.md` is the note that ships beside the zip and says so in their words. Real
notarisation needs a paid Developer ID; nothing else about the bundle would change.

**Apple Silicon only.** The binary is built for the host architecture and the bundled libraries
come from that Homebrew prefix, so an Intel Mac needs the whole thing rebuilt on (or cross-built
for) x86-64, with an x86-64 Homebrew to take the dylibs from. `lipo` cannot fix this after the
fact — there is no second set of libraries to fatten with.
