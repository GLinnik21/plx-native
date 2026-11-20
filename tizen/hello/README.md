# PlxNative on Samsung Tizen TV: step 1, "hello world" (+ probe H5, a static SDL2 + SDL2_ttf label)

A throwaway test build (branch `samsung/hello-tpk`, never merged). It is a `.tpk` that a 2021+
Samsung TV (Tizen 6.0+) or the Tizen TV emulator can install. A C# host loads our Rust `libplx.so`;
the library opens its own native window, clears it to a colour with EGL/GLES2, draws an **on-screen
diagnostics panel** (device facts, step results, the last key events, lifecycle lines), logs remote keys
and writes a log file. A tester with a retail TV and no log receiver photographs the panel: see
"Tester build".

It answers four questions:

1. **Does the host start?** Lines tagged `[host ...]` in the log; on screen, the panel exists at all.
2. **Does our .so load?** The `plx-hello 0.1.0` line (written by the .so); on the panel, step `1 .so`.
3. **Does the window draw?** The screen turns **orange** behind the panel and the log says
   `first frame presented`; on the panel, steps `3 window`, `4 EGL` and `5 GL`.
4. **Do keys arrive?** Every remote key is logged and listed on the panel, and **each key-down changes
   the colour** (orange, red, green, blue, yellow, magenta, cyan, white, back to orange). That works
   without reading any log.

**Probe H5** (section "H5: static SDL2 + SDL2_ttf" below) adds one more question on top of those four:
can our own `.so` carry a statically linked SDL2 and SDL2_ttf, with the window and EGL still ours, and
draw text with them? The panel is drawn through that path (panel step `6 SDL/TTF`), so it is also H5's
on-screen proof. It is additive: the colour and key behaviour above are unchanged.

**Probes H7 and H8** (sections "H7: the real renderer" and "H8: libcurl and the mock server" below) add
two more, each one line pair on the panel (steps `8 gfx` and `9 net`): does the application's real
GLES2 renderer (`plx_gfx`, the crate the app draws everything with) run on this set's GL (on the
emulator that is YaGL, GLES 2.0), and does the app's own libcurl binding load and reach a server on
the tester's PC. Both are additive and neither can crash the app or skip the panel.

Why this shape: retail Samsung TVs refuse a native `capp` package, so the package is `type="dotnet"`
and its C# host P/Invokes a native library (flutter-tizen's model). The host is a plain
`CoreUIApplication` whose `OnCreate` calls the library once on the main thread; the library creates
an Ecore_Wl2 window there (Ecore's main loop runs on that thread), registers key handlers there,
and a separate Rust thread owns the EGL context and draws.

## Tester build

For a tester with a **retail** Samsung TV and no log receiver: the app shows its own diagnostics panel
and the tester sends a photo of the screen. The network log (`--log-host`) stays optional.

**Build** (the signing environment variables are "Signing" below; without them the `.tpk` step stops
before building). Build from a committed and pushed tree: the panel header reads
`PlxNative Samsung probe <version> — <git short sha>` and says `+dirty` after the sha when anything
under `tizen/hello` differs from HEAD, so the photo names the code it came from.

```sh
tizen/hello/build.sh --arch arm            # real TVs (32-bit ARMv7): out/PlxHello-arm.tpk
tizen/hello/build.sh --arch x86_64         # emulator, on Linux/WSL2 (built by another session)
```

**Install.** The tester sideloads `out/PlxHello-arm.tpk` with Apps2Samsung. Apps2Samsung is said to
re-sign the package for the tester's own TV. **We have not verified that**, nor that it accepts a
`type="dotnet"` package signed with our profile (which lists only the DUIDs we know). If the install
fails, the tester's message and the TV model are the result.

**What the tester does.** Launch PlxHello. The screen shows the panel on a colour. Press, in any
order: the arrow keys, OK, Back, Play/Pause, Stop, rewind and fast-forward, red/green/yellow/blue,
the digits 0-9 and CH+/CH- (the instruction line says so). Each press shows in the panel's key
list and changes the colour. **Back does not exit** (nothing treats any key as quit); leave with the
TV's Home. Then photograph the screen. A second photo after pressing Home and re-opening the app
shows the lifecycle line (`pause +12.3s | resume, paused 2.8s +15.1s`). The first resume of a run, with
no pause before it (the app was opened, not left and re-entered), reads `resume (first) +1.3s`: only the
time since the start, never a pause length it does not have.

**What to send back:** the photo(s), the TV's model and year, and how it was installed (Apps2Samsung
or other) and whether it asked anything.

**The panel**, top to bottom (text 28 px on a 1080-line screen, re-rendered only when it changes):

- the header, with the version and git sha;
- the instruction line;
- the last 8 key events, oldest first: DOWN/UP, Ecore keyname, keycode (a key whose event carries no
  name shows as `(none)`); and below them every distinct key seen so far, `name=keycode`;
- lifecycle lines since start (`focus IN`, `pause`, `resume (first)` when nothing paused before it,
  `resume, paused 2.8s` after a real pause, locale changes), each with its `+Ns` since the start;
- the step results, green when they passed: `1 .so` (loaded, `plx_tizen_create` result), `2 libs`
  (which Ecore/EGL/GLES libraries opened), `3 window`, `4 EGL` (vendor, version), `5 GL` (vendor,
  renderer, version), `6 SDL/TTF` (the single `H5 RESULT`), `7 UTF-8` (a Latin + check mark + Cyrillic
  sample that must read `PlxNative H5 ✓ Привет`), `8 gfx` (H7, two to four lines, not cut at a
  character count below 400 per line: the verdict, the first shader error whole when a shader failed, the full list of
  missing GL extensions, the shader-bisect line and the shader-link line; the real renderer's own rectangle and text are also
  drawn in the bottom-right corner of the screen), `9 net` (H8, two lines: the GET's verdict, then which libcurl opened and its
  version);
- device facts: Tizen platform and native API version, model, manufacturer, build string, the four CPU
  architecture flags, `uname` machine, what `libplx.so` was built for, the .NET runtime, OS and process
  architecture, `IntPtr` size, the process executable and the glibc version.

The font has no glyph for the rewind/fast-forward symbols in the instruction line; the panel shows `<<`
and `>>` there (and says so nowhere else: it is not a failure).

**Reading the result without a log:**

| what the tester sees | what it means |
|---|---|
| the app never opens, or closes at once | the host or `libplx.so` failed before a window existed (the native create returned an error and the host exits). No panel is possible: a `--log-host` build, or the TV's own error text, is the next step |
| a plain colour, no text | the window and EGL work; SDL/TTF or the panel's GL did not (H5 failed). Same next step |
| the panel, a step in orange | the step named failed; the text after it is the reason |
| `8 gfx` orange, "plx_gfx not entered" | the real renderer was NOT started because it would have exited the process (see "H7"); the line names the shader pair, the stage and the driver's own first error line, and the last two `8 gfx` lines say which of the bisect's variants compile and which link variants link |
| `9 net` orange | libcurl did not load, or the GET failed; the line carries the `curl error N` text or the HTTP status |
| keys listed, colour changes | keys arrive; the codes to read off the photo |
| the panel frozen after Home | expected: the render loop idles while the app is in the background |

**Not verified (see also the last section).** The panel build has not run on any TV (the H5 probe before it passed on the TV 10.0 emulator, and the H7/H8 build ran three times on the x86_64 emulator inside WSL, as reported to us; this build has not run there). Whether
the media and colour keys reach an ordinary app at all is open: some TVs may deliver them only after the app
registers the key with the platform, which this build does not do, so a key missing from the photo
means "does not arrive unregistered", which is itself the answer wanted. The
Ecore key layout is read from the upstream EFL header; every pointer in it is checked before it is read.

## Layout

```
tizen/hello/
  plx/                  Rust crate -> libplx.so (own [workspace]; libc, plus path dependencies on the app's own
                        rust-modules/{base,net} and, with the `gfx` feature, rust-modules/gfx; nothing in rust-modules/ is changed)
  sdl/                  build-sdl.sh: static SDL2 + SDL2_ttf per arch (H5); layout-check.c, an ABI gate
  sdl-build/            its cache (gitignored): tarballs, per-arch include/ + lib/*.a + link-libs.txt
  host/PlxHello/        C# host + tizen-manifest.xml (package and app id: plxnhello0)
  build.sh              builds SDL, the .so and the .tpk (signed; --log-host adds the network log, --mock-host the H8 server)
  log-receiver.py       UDP receiver for the network log (`nc -klu PORT` also works)
  out/                  build output (gitignored): out/<arch>/libplx.so, out/PlxHello-<arch>.tpk
```

`libplx.so` links no Tizen library: `libecore_wl2`, `libecore`, `libecore_input`,
`libwayland-egl`, `libEGL` and `libGLESv2` are all `dlopen`ed and every attempt is logged.

## Build

| `--arch` | target | where it runs | built on |
|---|---|---|---|
| `arm` (default) | real TVs, 32-bit ARMv7 softfp | 2021+ sets | this Mac (webOS NDK gcc + glibc 2.12 sysroot) |
| `x86_64` | the **default emulator target**: TV Extension 10.0 ships only an x86_64 image | emulator | Linux, e.g. the tester's WSL2 |
| `i686` | the older 32-bit 9.0 emulator image | emulator | Linux, with `gcc-i686-linux-gnu` |

```sh
# The .tpk needs the signing variables ("Signing", below); without them these stop before building.
tizen/hello/build.sh                    # arm: out/arm/libplx.so, out/PlxHello-arm.tpk
tizen/hello/build.sh --arch x86_64      # in WSL2: out/PlxHello-x86_64.tpk
tizen/hello/build.sh --arch i686        # in WSL2, for the 9.0 emulator image
tizen/hello/build.sh --arch x86_64 --log-host 10.0.2.2:9999   # + network log ("The log", below)
tizen/hello/build.sh --arch x86_64 --log-host <PC-IP>:9999 --mock-host <PC-IP>:32499   # + H8's server ("H8")
tizen/hello/build.sh --arch x86_64 --so-only    # no .NET, no certificates (then --tpk-only elsewhere)
tizen/hello/build.sh --arch x86_64 --no-gfx     # without plx_gfx (H7 reports "no plx_gfx"): the fallback if the gfx link fails
```

`--mock-host HOST:PORT` (an IPv4 literal or a DNS name, a port 1-65535, no scheme and no path) writes
`res/mock-host.txt` into the package exactly as `--log-host` writes `res/log-host.txt`, and the build
checks the file is in the `.tpk` (and absent without the flag). `--no-gfx` builds the library without
the `gfx` cargo feature (and `--host-test` then skips its second, `--features gfx`, pass).

Prerequisites, all user-local, nothing system-wide:

- Rust via rustup (`arm` uses the repo's pinned toolchain, `rustup target add` is run for you).
- `arm` only: the webOS NDK at `~/webos-ndk/arm-webos-linux-gnueabi_sdk-buildroot` (`WEBOS_SDK` overrides).
- x86 on WSL2: `sudo apt install build-essential unzip curl` (and `gcc-i686-linux-gnu` for `i686`).
  `build-essential` is the C compiler, `make` and what the autotools-generated `configure` of SDL2 and
  SDL2_ttf need; `curl` and `unzip`/`tar` fetch and unpack the pinned source tarballs (the first build
  needs the internet; later builds use the cache). No `pkg-config`, `cmake` or `-dev` packages: the
  SDL build is configured with every optional backend off and links nothing from the build machine.
- `x86_64`/`i686` with the `gfx` feature (the default): also `python3` and `ar` (binutils, which
  `build-essential` brings). `plx_gfx`'s own build script (`rust-modules/build_support/host_link.rs`,
  written for the desktop simulator, run unchanged because `rust-modules/` is not touched) stages the
  pinned nanosvg headers with `python3 ci/deps.py stage nanosvg` (the first build needs the internet or
  the repo's dependency cache) and compiles `src/svg.c` with `cc`; see "H7", "How the x86 link works".
  `--no-gfx` needs none of that. Keep `libsdl2-dev` / `libsdl2-ttf-dev` off the build machine: with them
  installed the build script also puts the system `libSDL2.so` on the search path, and if that directory
  is searched before the one `build.sh` adds, the library gets a `DT_NEEDED libSDL2.so.0` and `build.sh`'s
  allowlist check fails (not verified which order the linker uses; it is the first thing to look at).
- .NET 8 SDK in `~/.dotnet`: `dotnet-install.sh --channel 8.0 --install-dir ~/.dotnet`. The package
  uses the legacy `Tizen.NET.Sdk` NuGet route (`tizen80`, which covers Tizen 6.0), not the
  `net6.0-tizen` workload, which only accepts Tizen 8+. NuGet restore needs the internet.

`build.sh` verifies the library and the package after building (file type, ELF attributes,
DT_NEEDED against a C-runtime-only allowlist, text relocations, highest glibc symbol version, exports,
that SDL/SDL_ttf/FreeType are inside the object and neither exported nor imported, that `plx_gfx` is
inside the object with its `gl*` imports answered by the library's own forwarders (or, with `--no-gfx`,
that none of it is), `unzip -l`, and `res/mock-host.txt` present in the package exactly when asked for)
and fails on a surprise. A library
needing a newer glibc than the target has (`PLX_MAX_GLIBC`) is a failure: on a WSL2 distro newer than
the emulator, see `X86_SYSROOT` in the script header.

## Signing

The `.tpk` must be signed with a **Samsung certificate profile**, and `build.sh` refuses to build it
without one (it names the missing variables). It never falls back to the SDK's bundled certificates:
Tizen.NET.Sdk 1.1.9's author certificate expired 2019-01-01 and its distributor ("Tizen Public
Distributor Signer") 2022-10-27, and a TV image trusts only Samsung "VD" distributor roots for
public/partner packages, so install fails with `Check certificate error` (118,-12) on any other
certificate. `--so-only` needs none of this.

1. Create the profile with the Samsung Certificate Extension (Tizen Studio's Certificate Manager, or
   the Tizen extension for VS Code). It needs a Samsung Developer login. Ask for a Samsung author
   certificate and a **Public** distributor certificate, and add the **DUID of every device** you will
   install on, the emulator and the TV (a device missing from the profile refuses the package). Where
   the extension leaves the files, and where it records the distributor password, is not verified here.
2. DUIDs: emulator `sdb shell 0 getduid` (plain `sdb shell` prints nothing on TV 10.0, see below). A
   retail TV has no sdb shell; the extension's device list reads its DUID over `sdb connect <tv-ip>`
   (not verified for a retail set).
3. Build with the profile's two `.p12` files and their passwords. Passwords are environment-only (no
   flag: shell history and `ps`), go to MSBuild as environment properties and are never printed;
   `*.p12`, `*.pfx`, `*.pem` and `*.key` are gitignored under `tizen/`:

```sh
export PLX_TIZEN_AUTHOR_P12=/path/author.p12      PLX_TIZEN_DIST_P12=/path/distributor.p12
read -rs PLX_TIZEN_AUTHOR_PASS; read -rs PLX_TIZEN_DIST_PASS   # no echo, not in history
export PLX_TIZEN_AUTHOR_PASS PLX_TIZEN_DIST_PASS
tizen/hello/build.sh --arch x86_64 --log-host 10.0.2.2:9999
```

The paths may be given as `--author-p12 FILE` / `--dist-p12 FILE` instead. A new device means a new
profile (with its DUID) and a repackage: `build.sh --arch X --tpk-only`, no recompiling.

Measured with throwaway self-made certificates on the Mac (no device): the SDK's signer reads a
`.p12` made with OpenSSL 3+ defaults (AES-256, SHA-256 MAC) as `PKCS12 key store MAC invalid - wrong
password or corrupted file` whatever the password; the same certificate re-exported with
`-certpbe PBE-SHA1-3DES -keypbe PBE-SHA1-3DES -macalg SHA1` signs. A Samsung-issued file that gives
that error with the right password needs the same re-export. Passwords with `% ; $ @ ( ) * ? '` are
escaped for MSBuild by `build.sh` and sign correctly.

## H5: static SDL2 + SDL2_ttf

**Question.** Inside the .NET host, can our `.so` use a statically linked SDL2 and SDL2_ttf (video
subsystem not used: the window and EGL stay ours via Ecore_Wl2) to do `SDL_Init` for the timer and
events subsystems, `TTF_Init`, open a bundled TrueType font, render a UTF-8 text label to an
`SDL_Surface`, upload it as a GLES2 texture and draw it on the existing frame from the render thread?

**What the library does** (in `plx/src/h5.rs`, `sdl.rs`, `label_gl.rs`, `panel.rs` and `panel_render.rs`;
every step logs its result and `SDL_GetError()` on failure; a failed step never stops the step 1
behaviour). Since the tester build the label the probe draws is the diagnostics panel; stage 1 is
unchanged and is what proves the SDL_ttf path before the panel uses it:

1. Main thread, in `plx_tizen_create` (after the log is up, before the Ecore/EGL libraries are opened):
   `SDL_SetHint(SDL_NO_SIGNAL_HANDLERS)` (SDL must not take SIGINT/SIGTERM from .NET), `SDL_Init(TIMER |
   EVENTS)`, a 20 ms `SDL_AddTimer` and an `SDL_PollEvent` drain, `TTF_Init`, `TTF_OpenFont` on the
   package's `res/appfont.ttf`, a check that the font has every character, `TTF_SizeUTF8`,
   `TTF_RenderUTF8_Blended` of `PlxNative H5 ✓ Привет` (Latin, a check mark and Cyrillic) at 72 px,
   then a blit onto an opaque dark plate and a conversion to RGBA bytes. Plain bytes leave this stage:
   no SDL object crosses a thread. `terminate` calls `TTF_Quit` and `SDL_Quit`.
2. Render thread (the EGL context owner): compile a textured-quad program, open the font itself
   (`TTF_OpenFont` is thread-agnostic here: only this thread uses it), rasterise the panel text
   (`panel.rs` is the text model; `panel_render.rs` lays it out at 28 px on a 1872x1048 plate authored for
   a 1080-line screen: 12 px of padding and a 2 px gap between sections, and a 31 px pitch between the
   rows of a wrapped line, which `TTF_SetFontLineSkip` sets (SDL_ttf 2.22 or later, the pinned build is
   2.24.0) without changing the 28 px glyphs. They were 16, 4 and the font's default, about 34 px; the
   step 8 lines written out in full no longer fit otherwise. Text may reach `CONTENT_LIMIT`, row 1036, and a
   test fails a panel that does not), `glTexImage2D` the RGBA bytes, and draw the quad (centred, scaled to the screen
   height) after every clear and before the swap. The panel texture is re-rendered and re-uploaded only
   when its text changed (a version counter; log lines `panel: render #N` and `H5 gl: texture N upload
   #K`), so a still panel costs one textured quad per frame. It logs the first presented frame with
   `glGetError`. A panel failure is not fatal: the colour screen carries on.

No SDL video driver is initialised: `SDL_INIT_VIDEO` is never requested, so there is nothing to choose
between the dummy driver and none. (The static SDL is configured with the dummy video driver only, so
even an accidental `SDL_Init(VIDEO)` could not open a display.)

**The font** is `pkg/appfont.ttf`, the app's own Inter (SIL OFL, already shipped by the app), copied with
`pkg/OFL.txt` into the package's `res/` by `build.sh` (generated, gitignored). The host passes
`DirectoryInfo.Resource` to `plx_tizen_create`, together with the data directory, the optional log host
and, since the tester build, the host's facts as `name=value` lines (the fourth parameter, `hostFacts`;
there is no new export: the same five `plx_tizen_*` names, one more argument, `Program.cs` in step).

**Versions.** `sdl/build-sdl.sh` pins SDL2 **2.32.10** and SDL2_ttf **2.24.0** by sha256 (a mismatch is a
hard failure) and builds both from the upstream tarballs with autotools. The main app links the webOS
NDK's *shared* SDL2, so there is no source pin of ours to reuse; these pins are this probe's own.
SDL2_ttf uses its **bundled FreeType 2.13.2** and has HarfBuzz **off** (no complex-script shaping;
Latin, Cyrillic and a check mark need none). SDL2 is configured minimal: dummy video only; no audio,
joystick, haptic, sensor, render, GL/GLES, X11, Wayland, KMS, udev, dbus, ibus or fcitx; `-fPIC`,
`-ffunction-sections`. SDL2_ttf's objects are repacked into a plain `libSDL2_ttf.a` (no libtool archive).

**How it is linked.** `plx/build.rs` links `libSDL2_ttf.a` then `libSDL2.a` by absolute path (not
`-lSDL2`: the NDK sysroot ships its own `libSDL2.a`), then the system libraries in `link-libs.txt`
(`-lm`, `-ldl`, `-lpthread`), then `-lc` again (glibc keeps `atexit` in `libc_nonshared.a`).
`PLX_SDL_PREFIX` selects the build; unset, the crate compiles a stub that logs `H5 probe: this
libplx.so was built WITHOUT static SDL` (plain `cargo build` and `cargo test` need no SDL). `build.sh`
always sets it, then checks `readelf -d` (no `libSDL2` and nothing else outside the C runtime), `nm`
(SDL/TTF/FreeType are local symbols, none in the dynamic table) and reports the size. ARM
`libplx.so`: 622,268 bytes before H5, 2,225,616 after.

**Building it.** One command builds SDL for the arch (cached in `sdl-build/<arch>/`, rebuilt when the
pins, compiler or configure flags change; `sdl/build-sdl.sh <arch> --force` rebuilds) and then the library:

```sh
tizen/hello/build.sh                              # arm, on the Mac (webOS NDK gcc, as step 1)
tizen/hello/build.sh --arch x86_64 --so-only      # WSL2/Linux: SDL + libplx.so, no .NET, no certificates
tizen/hello/build.sh --arch x86_64 --log-host 10.0.2.2:9999   # the full signed .tpk (signing variables set)
tizen/hello/build.sh --host-test                  # any host: the probe's unit tests against a host SDL build
```

On WSL2 keep the checkout on the Linux filesystem (`~/...`, not `/mnt/c`) at a path without spaces (the
SDL build refuses a path with spaces). The likeliest first-run failure on a new distro is the glibc
ceiling: SDL's and FreeType's C objects are compiled against the build machine's headers, and glibc 2.38+
headers turn calls like `strtol` into `__isoc23_strtol@GLIBC_2.38`. `build.sh` then stops with `GLIBC_2.xx
exceeds GLIBC_2.39` (the `x86_64` limit; `PLX_MAX_GLIBC` overrides it) and its message names the way out:
`X86_SYSROOT=<an older sysroot>` applies to the SDL compile as well as the Rust link. The first build downloads the two tarballs and compiles SDL2
and SDL2_ttf. `--host-test` builds SDL for the host (macOS or Linux) and runs `cargo test` twice (without, then with
`--features gfx`), which
renders the label and the diagnostics panel (a typical one, one with the emulator's expected `8 gfx` lines,
a long but plausible one, the longest step 8 the report can build, and an absurd one)
to surfaces without any GL and checks their pixels (`PLX_H5_DUMP=/some/file.bmp` writes the label and
`PLX_PANEL_DUMP=/some/stem` the panels as BMPs to look at). The script fails when either `cargo test` does
(the exit status of cargo itself, kept in a temp file: an earlier `cargo test | grep || true` followed by
`${PIPESTATUS[0]}` could never fail under `set -o pipefail`, because the `|| true` runs first and resets
`PIPESTATUS` to the `true`'s status; the fix was proven by a deliberately failing test, then removed).
The `gfx` pass sets `RUSTFLAGS=-L native=<the SDL prefix>/lib` as well: `plx_gfx`'s build script asks for
`-lSDL2 -lSDL2_ttf` on a non-ARM target, which on a machine with SDL installed linked that
SDL_ttf instead of the pinned static 2.24.0 this script built, and its text metrics (the wrapped-row pitch)
made the panel test measure a different panel than the device draws. The panel's 1040-of-1048 px
failure that showed this was a wrong message, not a wrong assertion: the test compared the bottom of
the text, with its trailing section gap, against the plate's height; the bound that decides is
`CONTENT_LIMIT` (the plate less its bottom padding, `panel_render.rs`), and the message now reports
headroom against that.

**Expect in the log** on success (device lines, in this order; `...` is elided detail):

```
H5 probe: static SDL2 + SDL2_ttf inside libplx.so; built against SDL2 2.32.10, SDL2_ttf 2.24.0 ...
H5 sdl: SDL_Init -> 0 (OK); SDL_WasInit(TIMER|EVENTS)=0x4001
H5 sdl: SDL_AddTimer(20 ms) id 1 returned after N ms ...; the callback ran 1x by N ms ...
H5 ttf: TTF_Init -> 0 (OK); SDL_ttf 2.24.0 (TTF_WasInit=1), FreeType 2.13.2, HarfBuzz 0.0.0 (0.0.0 = compiled out)
H5 ttf: TTF_OpenFont -> ok; family "Inter" style "Regular" height 88px
H5 ttf: TTF_RenderUTF8_Blended -> SDL_Surface 791x88 pitch ... in N ms
H5 sdl: stage 1 finished OK
panel: render #1 (content v...) N sections, rasterised in N ms, uploaded; text reaches row N of 1036 usable
H5 gl: texture N upload #1 1872x1048 RGBA (7847424 bytes), glError=0x0000
H5 gl: first frame presented with the panel drawn (glGetError=0x0000)
H5 RESULT: OK (static SDL2 + SDL2_ttf: SDL_Init, TTF_Init, font open, UTF-8 render to SDL_Surface, GLES2 texture upload and draw on the render thread; the diagnostics panel, with the UTF-8 sample, is on screen)
```

On screen: the step 1 colour with the dark panel plate in the centre (see "Tester build"; the sample
line `7 UTF-8` reads `PlxNative H5 ✓ Привет`), and the colour still changes on key presses. A failure logs
`H5 FAILED at <step>: <why>` (with the SDL or GL error text) and exactly one final `H5 RESULT: FAILED ...`
or `INCOMPLETE ...` line; the colour and keys still work, with no panel when SDL/TTF failed before it.
The size above (791x88) is what the host test measures for the stage-1 sample; the device may differ by
a pixel or two.

## H7: the real renderer on this set's GL

**Question.** Does `plx_gfx` (`rust-modules/gfx`, what the app draws every screen with) work on the
emulator's YaGL GLES 2.0 (and on a real set's GL): do its shaders compile, do its init functions run, do
its rectangle and text paths draw?

**What the library does** (`plx/src/gfx_probe.rs`, `gl_shim.rs`; `plx_gfx` is a path dependency behind
the `gfx` cargo feature, which `build.sh` turns on unless `--no-gfx`). On the render thread, in the EGL
context step 1 made current, from the frame after the panel's first (and after H5's result):

1. *Preflight* (below): compile and link every shader pair `plx_gfx` would, without exiting.
2. Only when all of them pass, the same four calls `rust-modules/src/app/boot.rs` makes at lines
   1730-1733, in that order: `plx_gfx::gfx::init_gl()`, `plx_gfx::text::init_text()`,
   `plx_gfx::gfx::init_image()`, `plx_gfx::gfx::init_blur()`. Each runs under `catch_unwind` and is
   logged before (`H7 gfx: entering init_gl`) and after (`H7 gfx: init_gl returned in N ms`), so a hang or
   a crash names the call.
3. Every frame after, over the panel, bottom-right corner: a rounded rect and the line `plx_gfx text`
   drawn by `plx_gfx`'s own `draw_rect` and `draw_text` (the text through its SDL_ttf glyph path, so a
   font that did not open shows as a missing line and a text width of 0). Frames are counted: step 8
   shows how many were drawn with `plx_gfx` (1 in the first result; after 60 frames, with no GL error and
   no call to a missing GL entry, the result is published once more with the count, and the log has
   `H7 RESULT@60: ...`). The two renderers share the one GL context, so the state is handed over by hand,
   as two lists in `gfx_probe.rs` (`swatch_state_ops`, `panel_state_ops`) that a test replays on a model of
   GL's state. Before the swatch the probe sets the viewport to the whole window, binds `plx_gfx`'s vertex
   buffer, points attribute 0 at it and enables it, enables straight blending, disables dither and the
   scissor test. After the swatch, after each init, and also when a draw or an init panics, it unbinds the
   buffer, disables attributes 0 and 1, blending and the scissor test, re-enables dither (GL's defaults,
   which is what the panel's draw, with its client-side arrays, was written against), selects texture
   unit 0 and puts the viewport back to the window (`init_blur`'s warm-up pass leaves the
   application's 1920x1080 viewport behind, and the panel sets its own only once, at start: on a
   window that is not 1080p the panel's next frame would draw into the wrong rectangle). A panel drawn
   with `plx_gfx`'s buffer still bound would read its arrays as offsets and vanish.
4. *The font*: step 8 says where `plx_gfx`'s text came from, `font from res/` or `font from the exe dir`
   (see "Findings", 3). With no text at all it says `TEXT DREW 0px` and where it looked.

**The panel line** (`8 gfx`, the verdict first; the whole text is also in the log, as `H7 RESULT: ...`
then `H7 RESULT+1: ...` for each further line). A healthy driver, after 60 frames (the first result, on
the first presented frame, reads `1 frame drawn with plx_gfx`; the log has it as `H7 RESULT:` and the
settled one as `H7 RESULT@60:`):

```
OK: init_gl+text+image+blur ran; shaders 14/14 compile+link; rect drawn, text 187px, font from res/
glGetError init/frame/swap 0/0/0; 60 frames drawn with plx_gfx; no GL ext: ARM|EXT_shader_framebuffer_fetch EXT_disjoint_timer_query
shader bisect: all 15 variants compile; j bytes clean
shader link: all 8 variants compile and link
```

Line 1 is the verdict: the four inits ran (a panicked one is named with its message), the pair tally,
the rectangle and the text's width in pixels, and where the text's font came from. Line 2 is the three
`glGetError` reads, the frames drawn with `plx_gfx`, anything that went wrong later (a GL error on a
later frame, a call to a GL entry the library lacks, `plx_gfx`'s own failure lines, no font) and the
missing extensions. The last two lines are the bisect and the link variants. After a late fault the step
turns orange at once and there is no settled report.

A refusal (a shader the driver rejects; `plx_gfx` is not entered):

```
FAILED, plx_gfx not entered: 1 of 14 shader pairs do not compile+link (init would exit)
first failure init_gl ambient, vertex stage: 0:37(9): error: syntax error, unexpected NEW_IDENTIFIER
shaders 13/14 compile+link; no GL ext: ARM|EXT_shader_framebuffer_fetch EXT_disjoint_timer_query
shader bisect (+ compiles, - fails): a0- a- b- c+ d- ... ; j bytes clean
shader link: L1:vs- L2:link- L3:vs- L4:link- ...
```

(Both samples are illustrative, not a recorded panel: the first is the shape the failure had on the first
emulator run, the last two are what a refusal would read like if the split did NOT help, with `a` failing
like `a0`, `L1` failing in the vertex stage and `L2` at the link.) Nothing on these lines is cut at a character count except a runaway line over 400 characters (`STEP_LINE_MAX` in `panel.rs`, a guard that ends the line with `...`; the worst-case panel test fixture is long enough to hit it): the panel wraps at its width and
the log splits any text longer than 1000 bytes into `(cont.) ` lines so that no UDP datagram is truncated
(the log's `plog_full!`, `log.rs`); an earlier build cut the second line to "...syntax error,...; no G...".
When the driver's info log has further lines the first-failure line ends `(+N more log lines in the UDP
log)` (all of them are in the log, under `H7 gfx:   info log:`).

`no GL ext:` lists the extensions the renderer uses when present that this GL does not list: the
ARM or EXT framebuffer fetch its straighten pass reads in place (without it the pass copies the frame
instead), and the disjoint timer query its GPU timing uses (without it timing stays off); `none` when
all are there. They are information, not a failure. The
`glGetError` triple is after the inits, after the frame's draw and after the swap, `0` = none, else
`0x....`. The GL strings and the full extension list are logged (`H7 gfx: GL_VERSION ... GLSL ...`,
`H7 gfx: GL_EXTENSIONS ...`), and so is each shader pair (`H7 gfx: shader pair "init_gl base rect"
compiled and linked`, or `... FAILED` and then the driver's whole info log, a line each, under
`H7 gfx:   info log:`). A failure reads `FAILED, plx_gfx not
entered: <why>` and the app carries on with the colour and the panel.

**The shader bisect and the link variants** (`plx/src/shader_bisect.rs`). The first emulator run (YaGL
GLESv2, "OpenGL ES GLSL ES 1.4") compiled 13 of the 14 pairs; `init_gl ambient` (`VS_AMBIENT_DITHERED` +
`FS_AMBIENT`) failed in the vertex stage with `0:37(9): error: syntax error, unexpected NEW_IDENTIFIER`
(Mesa's error format: source 0, line 37, column 9, which is the `highp` of `varying highp vec2
v_dither_nc;`: `varying ` is eight characters). The same declaration compiles in two sibling shaders. This
step compiles the source in variants, and then compiles and LINKS whole pairs, to find out what the driver
needs. It runs after the preflight, whether or not the preflight passed, never enters `plx_gfx`, and each
trial is logged before it runs, so a crash names the variant.

**What the emulator said so far** (as reported to us; none of it is a claim about a TV):

- *Second run, `e4ae272f1` (the bisect, `plx_gfx` sending no version line):*
  `a- b- c+ d- e- f- g+ h- i- k- l+ m- n- o+; j bytes clean`. `a` fails exactly like `m`, so the failure is
  `plx_gfx`'s own two-string call and not the probe's one-string one; the source compiles only with
  `#version 100` first (`g`), without that `highp` (`c`), or without any precision qualifier (`o`); no
  comment, define combination or byte in the source matters (`b`, `d`, `e`, `f`, `k` fail the same, `j` is
  clean). `glGetShaderSource` returned 8 bytes for every variant: a stub.
- *Third run, `8a5998fb0` (`plx_gfx` sending `#version 100`):* the three pairs built from `vs_ambient.vert`
  (ambient, scrim layer, ambient plain) now COMPILE and fail to LINK with `error: all shaders must use same
  shading language version`. 11 of 14 pairs passed, `plx_gfx` was not entered. The working hypothesis, not measured: YaGL
  translates most sources and hands `vs_ambient.vert` to the host compiler raw (desktop GLSL 1.10 with no
  version line, where `highp` is an identifier; ES 1.00 behind `#version 100`, next to a fragment source
  that was translated), and Mesa's linker wants every stage at one language version.
- *The hypothesis this build tests:* `vs_ambient.vert:28`, `uniform vec4 u_atl, u_atr, u_abr, u_abl;`, is
  the only statement with several declarators in all of `rust-modules/gfx/src/shaders` (a test checks
  that), and the shortest failing variant (`i`) contains it. This branch splits it into four declarations
  (`rust-modules/gfx/src/shaders/vs_ambient.vert`, and nothing else in `rust-modules/` differs from the
  second run's tree; a separate pull request does the same on `main`), with `plx_gfx`'s ES preamble the EMPTY
  string again, and measures before and after in one run. The probe's `ES_PREAMBLE` is `""` once more (a
  test reads `gfx.rs` and fails if they drift); `#version 100` lives on as the literal `VERSION_100` that the
  variants hand over on purpose.

**What is compared.** The rule: the plain-letter ladder is built from the repository's CURRENT (split)
`vs_ambient.vert`, what `plx_gfx` compiles now; the ORIGINAL is a frozen, byte-for-byte copy
(`plx/src/probe_shaders/vs_ambient_unsplit.vert`, taken from git before the split; a test fails unless it
equals the repository's file with the four declarations joined into one statement again) that the original
side of every pair below is built from. Every compile hands GL two strings like `plx_gfx` (an empty first
string, then the source) except `m` (one string) and the variants that name a `#version 100` first string.

The ladder, compile only (`X` below is the id; the source is `VS_AMBIENT_DITHERED`: 3 `#define` lines, then
`vs_ambient.vert`), the pair `a0` / `a` being the before and after at the full-source level:

| id | the source |
|---|---|
| a0 | BEFORE: the ORIGINAL source (the unsplit statement), behind `plx_gfx`'s own two-string call: `a` as the first two runs measured it |
| a | AFTER: as is, the repository's split source, behind `plx_gfx`'s own two-string call: what it compiles now |
| b | every `//` comment line removed (line numbers shift) |
| k | the comment text blanked, every line kept (line numbers unchanged: a comment character is ruled in or out without moving the error) |
| c | `highp` dropped from the `varying highp vec2 v_dither_nc;` line only |
| d | only `#define PLX_DITHER_NC` (the other defines gone) |
| e | `PLX_DITHER_NC` + `PLX_WASH_INK` |
| f | `PLX_DITHER_NC` + `PLX_SCRIM` |
| g | `#version 100` as the first line of the source (the compile-only twin of `L2`'s vertex stage) |
| h | the text after `#ifdef` resolution (no directive left) |
| i | everything through the `v_dither_nc` block (its closing `#endif` included, so the text is 42 lines (39 on the original), not 35: cutting at 35 would leave an `#ifdef` open and fail for another reason), then `void main(){ gl_Position = vec4(a_pos,0.,1.); v_col = vec4(1.); v_dither_nc = a_pos; }` |
| l | control: `VS_SRC_DITHERED` (the sibling that compiled in the first run) |
| m | `a` again, but ONE source string: what the first run's preflight passed |
| n | the `v_dither_nc` declaration moved to the top of the source |
| o | every precision qualifier removed |
| j | not a compile: the bytes of `a`, counted for CR, TAB, a leading BOM, NUL and non-ASCII (the log line gives all five; the panel summary says `j bytes clean`, or `j bytes: <what>` for a CR, TAB, BOM or NUL; non-ASCII alone is ordinary here, the comments have dashes) |

(`p`, the variant the third run's build added for `plx_gfx`'s own call with the `#version 100` preamble, is
gone: with the preamble empty again it is the same call as `a`.)

The link variants: each compiles a vertex AND a fragment source (`FS_AMBIENT`: `dither.glsl` then
`fs_ambient.frag`, which has a `varying highp` of its own and reads `v_col`) and links them, with `a_pos`
bound to attribute 0 as `link_shaders` does. A stage that does not compile does not stop the other one from
compiling; with either failed nothing is linked.

| id | vertex source, first string | what it asks |
|---|---|---|
| L1 | the split `VS_AMBIENT_DITHERED`, empty on both stages | what `plx_gfx` does now: expected to link if the split is the cause |
| L2 | L1, `#version 100\n` as the first string of BOTH stages | does the split source survive a version line on both stages |
| L3 | the ORIGINAL unsplit source, empty on both stages | expected: the vertex stage does not compile (the first run) |
| L4 | the ORIGINAL with `#version 100\n` on both stages | expected: both compile, the link fails (the third run) |
| L5 | cross-link: `VS_SRC_DITHERED` + `FS_AMBIENT`, empty | does a vertex stage that compiles link with this fragment. `vs_src.vert` declares no `v_col`, which `fs_ambient.frag` reads, so a failure here counts only if the log names VERSIONS, not `v_col` |
| L5v | L5 with `varying vec4 v_col;` declared and written by the vertex source | the same pair made complete: its failure is not the missing varying |
| i0 | variant `i` cut from the ORIGINAL + `FS_AMBIENT`, empty | the shortest failing source, before |
| i2 | variant `i` cut from the SPLIT source + `FS_AMBIENT`, empty | the shortest failing source with the split applied (a trivial `main` writes both varyings the fragment reads) |

For each compile the log has, in order:

```
H7 bisect X: compiling (two strings): <what>
H7 bisect X: <N> bytes, <M> lines -> COMPILED|FAILED; passed <N> bytes + NUL; GL_SHADER_SOURCE_LENGTH <n>; glGetShaderSource returned <n> bytes, identical|DIFFERENT from what was passed (first difference at byte <b>)
H7 bisect X info log: <each line of the driver's whole info log>
H7 bisect X: the log blames line <n>: <that line of the variant's text>
```

`GL_SHADER_SOURCE_LENGTH` and the `glGetShaderSource` read-back were meant to answer whether YaGL truncates
or alters the text it was handed. **On YaGL `glGetShaderSource` is a stub**: it returned 8 bytes for every
variant, the compiling ones included. A read-back under 16 bytes for a source of at least 16 is therefore
logged as `glGetShaderSource UNSUPPORTED here (returned 8 bytes for N passed: a stub, so it says nothing
about the kept text)` and is not counted as "different" (the panel summary names a read-back only when
it is trustworthy and differs). `the log blames line n` counts in the text the driver composed from the
strings, so with a `#version 100\n` first string the blamed line is the source's line plus one. Then
`H7 bisect j: ...` (the byte checks) and `H7 bisect: <summary>`, which is also a panel line. For each link
variant:

```
H7 link X: compiling both stages and linking (an empty first string on both stages | `#version 100` as the first string of both stages): <what>
H7 link X: vertex COMPILED|FAILED (<N> bytes), fragment COMPILED|FAILED (<M> bytes), link LINKED|FAILED|not attempted (a stage did not compile) => <how it ended>
H7 link X vertex|fragment|link info log: <each line of that info log, whole; read on success too, because a driver's warnings are there; a stage or link that failed with nothing to say reads "(the driver gave no info log)">
H7 link X vertex|fragment: the log blames line <n>: <that line of the stage's text>
```

and last `H7 link: <summary>`. The two panel lines (the last two of step 8):

```
shader bisect (+ compiles, - fails): a0- a+ b+ c+ d+ e+ f+ g+ h+ i+ k+ l+ m+ n+ o+; j bytes clean
shader link: L1+ L2+ L3:vs- L4:link- L5:link- L5v+ i0:vs- i2+
```

(`+` compiled or linked, `-` did not; on the link line `vs-` / `fs-` / `both-` mean that stage (or both) did
not compile and `link-` that both compiled and the link failed. Everything compiling and linking reads
`shader bisect: all 15 variants compile; j bytes clean` and `shader link: all 8 variants compile and link`.)
Both samples are **what the split hypothesis predicts**, not a recorded panel: they are the output of the
unit tests' model of it.

Reading it, if the hypothesis holds: `a0-` with `a+` is the split being what matters at the full-source
level, `i0:vs-` with `i2+` at the shortest-source level; `L1+` is the renderer working; `L3:vs-` and
`L4:link-` the two failures the earlier runs saw, reproduced in this run. If instead `a-` or `L1` does not
link, the split is not the cause: `c+`, `g+` and `o+` as before say the precision qualifier and the version
line still matter, and the `H7 link L1 link info log:` lines say what the linker objects to. `L2-` with
`L1+` means a version line on both stages is wrong even for a translated source and `L2+` that it is
harmless. `L5:link-` with `L5v+` was only the missing varying; `L5v:link-` with the log naming versions
means YaGL's stages disagree on language version even for sources it translates. If every variant
including the control `l` fails the stage itself is broken.

**Findings.** Nothing in them changes `rust-modules/` except the split declaration in the last finding,
which this test branch carries (a separate pull request makes it on `main`):

1. **`plx_gfx` has no entry that survives a failed shader.** A required shader that does not compile goes
   `link_program` -> `gfx_compile` -> `plx_base::process::exit_now(1)` (`gfx.rs:1231-1232`), and a base
   program that does not link does the same in `init_gl` (`gfx.rs:1451`). `exit_now` is `_exit`: no panic
   to catch, no destructor, and the .NET host dies with it, taking the panel (the only way to read the
   result without a log) along. The non-exiting pieces (`try_compile`, `link_optional_program`,
   `link_shaders`) are private. So the probe does its own **preflight**: the 14 vertex+fragment pairs the
   four init functions pass to `link_program`, composed from the very same `include_str!` shader sources
   (`gfx_probe.rs`, `PAIRS`), are compiled and linked with the probe's own non-exiting GL calls (through `glShaderSource` with the same
   two strings `plx_gfx` passes: its preamble, the empty string on an ES context, and the
   source; the first emulator run's preflight passed ONE string and no preamble), and
   `plx_gfx` is entered only if every one passes, the GLSL version string is an ES one (`plx_gfx`
   prepends a `#version` line by the GL kind it detects) and every GL function it imports exists in the
   opened library. A test reads `gfx.rs` and `text.rs` and fails if a shader pair the renderer
   passes to `link_program` is missing from the table, so a shader added later cannot silently escape the
   preflight. Those two sites are the only exits: `exit_now` has no other caller in `rust-modules/gfx`,
   `plx_machine` or `plx_base`, nothing there calls `process::exit` or `abort`, and `link_program` (the
   exiting kind) has exactly 14 call sites, 8 in `init_gl`, 1 in `init_image`, 2 in `blur_lazy_init` (which
   `init_blur` runs at once) and 3 in `init_text`; every other program the renderer builds, lazily at draw time included, goes through
   `link_optional_program` and degrades. If the real renderer's own compile still fails after the
   preflight passed (the same source, so not expected) the process exits and the log ends at
   `H7 gfx: entering init_gl`.
2. **`plx_gfx` links its GL at build time (`glClear`, ... as plain imports); `libplx.so` must not have a
   `DT_NEEDED` for the GL library** (step 1 opens libEGL/libGLESv2 itself, and the cdylib is `BIND_NOW`, so
   one unresolved `gl*` name would make `dlopen("libplx.so")` fail and the whole app with it). The
   library therefore defines the 50 GL entry points `plx_gfx` imports itself (`gl_shim.rs`) as
   forwarders to function pointers `dlsym`ed from the library step 1 opened (library-scoped, never
   `RTLD_DEFAULT`). A missing entry is recorded and refuses to enter `plx_gfx`; a test fails when
   `gfx.rs`, `text.rs`, `egl.rs` or `gpu_timer.rs` start importing a function the shim lacks.
   `build.sh` checks the product: none of them undefined, the forwarders exported.
3. **The font.** `plx_gfx` opens `<dir of the running executable>/appfont.ttf`; the executable here is
   `dotnet`/the launcher, so the open fails and no text is drawn. On Linux the build links with
   `-Wl,--wrap=TTF_OpenFont` (`plx/build.rs`) and the probe's `__wrap_TTF_OpenFont` retries a failed
   `appfont*.ttf` open from the package's `res/` (the H5 probe's font). macOS's linker has no `--wrap`,
   so the host test build leaves it out (the wrapper is not compiled there). Step 8 reports the outcome
   in its first line: `font from res/` when the redirect opened the face (the log then has
   `H7 font: "<exe dir>/appfont.ttf" did not open; retried from the package: "<res>/appfont.ttf" ->
   opened`), `font from the exe dir` when `plx_gfx`'s own first path worked, and `TEXT DREW 0px` with
   `no font face, looked in <the path>` when neither did. (`from res/` is the redirect's counter, so it
   is only ever claimed on a Linux build, where the wrapper exists.)
4. **Event ring.** `plx_gfx` writes its own lines (`plx_base::eventlog::log`, e.g. `base prog link
   FAILED — exiting` just before the exit above) into `plx_base`'s event ring, which keeps records
   here because the crate is built with its `lab-diagnostics` feature; the probe forwards new ring
   records to the log as `evt +Nms ...` lines, so the renderer's own words reach the UDP receiver.
5. **`vs_ambient.vert` declares four uniforms in one statement, and YaGL does not take it** (the
   emulator's measurement; see "The shader bisect and the link variants"). The renderer's shaders have
   exactly one statement with several declarators, `uniform vec4 u_atl, u_atr, u_abr, u_abl;` (a test
   checks that). On YaGL the vertex stage built from that file did not compile (`highp` read as an
   identifier) unless the source began with `#version 100`, and with `#version 100` the three pairs built
   from it compiled but did not link ("all shaders must use same shading language version"). This branch
   splits the statement into four declarations, the only change in `rust-modules/`
   (`gfx/src/shaders/vs_ambient.vert`; a separate pull request makes it on `main`), and keeps
   `glsl_preamble`'s ES branch as it always was, `return c"";`. A `#version 100` preamble was tried for one
   run (`1a7e3e3af`, `8a5998fb0`) and reverted. Whether the split is what matters is what this build
   measures (the pairs `a0`/`a`, `L3`/`L1`, `L4`/`L2`, `i0`/`i2`). The television's own Mali compiled the
   original; nothing here says it minds four declarations.

**How the x86 link works (not built or run on the machine this was written on).** On a non-ARM target
`plx_gfx`'s build script emits `-lSDL2 -lSDL2_ttf -lGL` and, as a link argument that does not reach
dependents, an object file built from `src/svg.c`; ARM emits nothing. `build.sh` handles the first two
for the x86 build: it puts the SDL prefix's STATIC archives on the library search path (`-L native=`) so
`-lSDL2` / `-lSDL2_ttf` find the `.a` files this script built instead of a shared library, and creates
an empty `libGL.a` there so `-lGL` is satisfied by nothing (the GL names are the library's own
forwarders). The svg object does not reach the `libplx.so` link: its nanosvg symbols stay undefined in
`plx_gfx` and are dropped with the code that uses them (the ARM build, which has the same property, shows
no undefined `svg` symbol). If this does not link or `build.sh`'s allowlist check fails, **`--no-gfx`**
builds the library without `plx_gfx` and the panel's `8 gfx` says so.

**Dependencies.** `cargo tree --features gfx` (ARM target, normal edges) adds 20 packages to the tree
without the feature: `plx_gfx`, `plx_machine` (-> `plx_base`) and `image` 0.25 with `png`, `zune-jpeg`,
`moxcms`, `flate2`, `miniz_oxide` (in two versions, 0.8.9 and 0.9.1), `crc32fast`, `cfg-if`, `fdeflate`,
`bytemuck`, `byteorder-lite`, `num-traits`, `bitflags`, `simd-adler32`, `adler2`, `zune-core` and `pxfm`.
The deepest chain is plx_gfx -> image -> png -> flate2 -> miniz_oxide -> adler2 (five levels below
`plx_gfx`). All pure Rust: no FFmpeg, Sentry or webOS-only link. (The ARM `libplx.so` grows from
2,749,312 to 3,079,212 bytes.)

## H8: libcurl and the mock server

**Question.** Does the app's own libcurl binding (`plx_net`, the crate behind every Plex request the
app makes, on `plx_base`'s `dynlib!` loader) find a libcurl on this set under one of the app's SONAMEs, and
can it make an HTTP request to a server on the tester's PC?

**What the library does** (`plx/src/net_probe.rs`). At the end of `plx_tizen_create`:

1. `plx_net::net::curl::load(None)`: the app's own candidate list, `libcurl.so.4`, `libcurl.so.5`,
   `libcurl.4.dylib` (`rust-modules/net/src/net.rs`; a test reads that file and fails if the probe's copy
   of the list, used for the panel's "tried ..." text, drifts). Nothing is linked: `libplx.so` has no
   `DT_NEEDED` for curl. The panel and log say which name opened, or that none did, or that one opened
   with symbols missing.
2. `plx_net::net::global_init()` (what the app calls once on its main thread: `curl_global_init`, the
   OpenSSL locks where the TLS backend needs them) and `plx_net::net::curl_version()`, shown as the
   version string (it names the TLS backend).
3. If the package has `res/mock-host.txt`: on its own thread (`plx-net-probe`), `plx_net::net::
   request_result_evidence` (the one function every libcurl easy request in the app passes through)
   `GET http://<mock-host>/identity`, and the HTTP status and the `machineIdentifier` from the JSON
   body, or the `CURLcode` and its words. Without the file nothing is requested and the panel says so.

**The panel line** (`9 net`, two lines; log lines `H8 net: ...`, the final `H8 RESULT: ...`):

```
OK: GET http://<PC-IP>:32499/identity -> 200, machineIdentifier s2265b1f5s91b7584a
libcurl libcurl.so.4: libcurl/7.xx.x OpenSSL/... zlib/...
FAILED: GET http://<PC-IP>:32499/identity: curl error 7: failed to connect to host     (no server, or a firewall)
libcurl NOT loaded: none opened (tried libcurl.so.4 libcurl.so.5 libcurl.4.dylib)      (no libcurl on this set)
libcurl LOADED; no GET made (this package has no mock host: build with --mock-host)
```

**The server.** `tests/mock_pms.py` (the repo's mock Plex Media Server; its `/identity` is tokenless
JSON with a `machineIdentifier`; the id it returns is synthetic, from a seed). It binds `127.0.0.1` by
default. Whether that is reachable depends on where the client is:

- **A Tizen emulator running inside WSL** (the run this probe was first tried on): QEMU's user-mode
  networking maps the guest's `10.0.2.2` to the loopback of the machine the emulator runs on, here
  WSL's, so a mock bound to `127.0.0.1` IS reachable. Build with `--mock-host 10.0.2.2:32499` and start
  `python3 tests/mock_pms.py --host 127.0.0.1 --port 32499` inside WSL (the first run did exactly this: the
  GET returned 200). An earlier version of this README said a loopback mock could not be reached from the
  emulator; that was wrong for this setup.
- **A real TV, or an emulator on another machine than the mock:** the loopback address is the mock's
  machine only, so bind all interfaces, from a checkout of this branch on the machine named in
  `--mock-host`:

```sh
python3 tests/mock_pms.py --host 0.0.0.0 --port 32499     # serves http://<PC-IP>:32499/identity; Ctrl-C stops it
curl http://<PC-IP>:32499/identity                         # from the same machine: must print machineIdentifier
```

For the real-TV case allow inbound TCP 32499 through that machine's firewall, and give `--mock-host` an
address the TV can route to (`<PC-IP>`, the machine's LAN address). WSL2 behind its own NAT is a third
case for a real TV (not verified here): the address has to be a Windows host address with the port
reachable on it (WSL2's mirrored networking mode, or a `netsh interface portproxy` from Windows to the
WSL2 address). `10.0.2.2` is only the emulator's name for its host, as for `--log-host`.
`mock_pms.py` is a long-running local server, so stop it (Ctrl-C) when the run is done.

## Install and run

**Real TV (Developer Mode on, its DUID in the profile):** `sdb connect <tv-ip>`, then
`sdb install out/PlxHello-arm.tpk` (Apps2Samsung also sideloads; not tried with a signed package).
Launch "PlxHello" from the TV's apps.

**Emulator:** the Tizen SDK 10.0 CLI installer plus `package-manager-cli` with **TV Extension 10.0**
(Tizen Studio, the IDE, is deprecated). Then, with the emulator running:

```sh
sdb install out/PlxHello-x86_64.tpk       # i686 for the 9.0 image
sdb shell 0 execute plxnhello0            # or launch it from the app list
```

The emulator cannot test `Tizen.Multimedia` playback; this step does not need it.

## The log

The file is `<app data dir>/plx-hello.log`, appended and flushed per line, written by both the host
(`[host ...]`) and the library (`[plx +secs tTID]`). The data dir is the host's `DirectoryInfo.Data`
(its path is the first host line); typically `/opt/usr/home/owner/apps_rw/plxnhello0/data/`.

**That file cannot be read from the device.** On the TV 10.0 emulator `sdb shell <cmd>` prints nothing
(only `sdb shell 0 <cmd>` works), `sdb dlog` is empty and `sdb pull` of the app's data dir is
refused; a retail TV has no sdb shell and no dlog at all. So the log leaves the device over the
network: `--log-host IPV4:PORT` writes the address into the package (`res/log-host.txt`, generated,
gitignored) and the app sends **every log line as one UDP datagram** to it: the library's lines as
`<seq> <line>`, the C# host's own lines (so a failed .so load still shows) as `h<seq> <line>`. `seq`
counts per sender and per line, so a gap is a lost datagram. Sends are non-blocking and failures are
ignored; without `--log-host` nothing is sent. IPv4 literal only (no name lookup).

Receive on the machine named in the address, before launching the app:

```sh
nc -klu 9999                       # raw lines; or: python3 tizen/hello/log-receiver.py 9999 (marks gaps)
```

From the emulator the host machine is usually `10.0.2.2` (QEMU's user-mode network); from a real TV use
the LAN address of the receiving machine, on the same network, with inbound UDP allowed through its
firewall. The first lines say `network log: UDP -> ...` or why it is off. If nothing arrives, the
screen colour (below) still answers questions 1 to 4.

## What to report

1. How you launched it (`sdb`, Apps2Samsung, or the TV's own app menu), whether the app started (host)
   and whether anything appeared. Did datagrams arrive at the receiver? Paste its output (it is the log).
2. Did the .so load? If not, paste the `[host ...]` lines (`DllNotFoundException`, the `preload dlopen
   ... FAILED:` text is the loader's own reason).
3. Did the window turn orange? Did it stay on screen?
4. Do keys arrive? Press arrows, OK, Back, colour buttons, media keys: does the colour change?
   Paste the `key ...` lines, the `key raw event` lines and any `window focus` lines.
5. The `OnPause`, `OnResume`, `OnTerminate` and `lifecycle:` lines. Nuvio's notes record Tizen 8/9
   pausing an app about 3 s after a launch from the TV menu (not from Apps2Samsung); a pause freezes the
   colour and looks like a failed draw.
6. These lines, always: `uname machine=`, `arch(compile-time)`, `glibc`, the `platform.version` and
   `model_name` host lines, `cpuinfo`, `EGL vendor=`, `GL vendor=/renderer=`, `screen size`.
7. H5: every `H5 ...` line, in particular the single `H5 RESULT:` line, and whether the label is
   readable on screen (check mark and Cyrillic included).
8. If it failed: the `plx_tizen_create FAILED -> N (NAME)` line. Codes are in `plx/src/lib.rs`
   (`code`): 4 a library did not open, 5 a symbol is missing (the `MISSING (required)` lines name it),
   6 to 11 Ecore/window/key handlers, 12 to 16 EGL, 17 to 19 the render thread. Host-side: -1 .so not
   found, -2 export missing, -3 other.
9. H7: the panel's `8 gfx` lines (all of them, the last two are the shader-bisect and shader-link
   summaries), the log's
   `H7 ...` lines (`H7 RESULT:` and its `RESULT+N:` continuations, the settled `H7 RESULT@60:` after 60
   frames, `GL_VERSION`, `GL_EXTENSIONS`, each `shader pair` and its `info log:`, `entering ...` and
   `... returned in N ms`, `H7 font: ...`, `H7 gfx: glGetError after init = ...`, `... after the draw = ...`) and, whether or not a shader failed, every
   `H7 bisect ...` line (the compile result, the info log and the read-back per variant, then the
   `H7 bisect:` summary) and every `H7 link ...` line (each variant's stages, link and whole info logs,
   then the `H7 link:` summary), and whether the PANEL STILL DRAWS after the inits (the whole panel, not just the
   corner) and whether the rounded rectangle and the `plx_gfx text` line are on screen in the
   bottom-right corner. If the log stops after `H7 gfx: entering <call>`, the
   process died inside that call (no panel then): that line is the finding.
10. H8: the two `9 net` lines and the `H8 ...` log lines. A `curl error N` is `CURLcode` N (7 = could not
    connect: the server, the address or the firewall; 28 = timeout; 6 = name not resolved).

## Unverified (the panel build has not run on a TV; the H5 probe before it passed on the TV 10.0 emulator, and earlier H7/H8 builds ran three times on the x86_64 emulator inside WSL, as reported to us; this build has not)

**What the first emulator run reported** (x86_64 emulator inside WSL, the build before the shader
bisect; reported to us, not run by the author): H8 passed (libcurl loaded; `GET http://10.0.2.2:32499/identity`
-> 200 against a mock bound to `127.0.0.1`); the x86_64 link with `plx_gfx` was clean and all 50 GL
forwarders bound; the GL is "YaGL GLESv2", "OpenGL ES 2.0", GLSL "OpenGL ES GLSL ES 1.4"; H7's preflight
compiled and linked 13 of 14 shader pairs and refused: `init_gl ambient`, vertex stage,
`0:37(9): error: syntax error, unexpected NEW_IDENTIFIER`, which is the `varying highp vec2
v_dither_nc;` declaration (`vs_ambient.vert:34`, composed line 37 after three `#define` lines); the same
declaration compiles in `VS_SRC_DITHERED` and `VS_IMG_DITHERED`; non-ASCII bytes in the comments were
ruled out by a look; the app stayed healthy (about 31 fps, `glGetError` 0).

**What the second emulator run reported** (the build with the shader bisect, `e4ae272f1`): the bisect
printed `a- b- c+ d- e- f- g+ h- i- k- l+ m- n- o+; j bytes clean` (see "The shader bisect and the link variants": the source compiles
only with `#version 100` first, without that `highp`, or without any precision qualifier; `a` fails like `m`);
`glGetShaderSource` returned 8 bytes for every variant, so it is a stub on YaGL; the host tests passed
(110 and 114, with the pinned static SDL confirmed by `ldd` and `nm`) and the x86 link was clean.

**What the third emulator run reported** (`8a5998fb0`, `plx_gfx` sending `#version 100`): the pairs built
from `vs_ambient.vert` (ambient, scrim layer, ambient plain) compile and all three fail to LINK, `error: all
shaders must use same shading language version`; 11 of 14 pairs passed, `plx_gfx` was not entered, and the
bisect read `a- ... o+ p+` (see "The shader bisect and the link variants"). This build reverts that
preamble and splits `vs_ambient.vert`'s multi-declarator uniform statement (finding 5), so if all 14 pairs
pass it ENTERS `plx_gfx` for the first time: nothing about `init_gl`, `init_text`, `init_image`,
`init_blur`, the swatch or the panel after them is measured yet. Nothing of these runs is a claim about a TV.

H5, the tester-build panel, H7, the shader bisect and link variants and H8, on the Mac (no Tizen device, no
Linux), only these were run:
`build.sh --host-test` (131 tests with SDL, then 135 with `--features gfx`: the label and the panel render
to surfaces with the right pixels, the panel fits at 28 px with typical, emulator-shaped, long and
worst-case contents including the new steps (against `CONTENT_LIMIT`, with the pinned static SDL_ttf), the key-string guard refuses unmapped and `PROT_NONE` pointers, the create path logs one
line per fact; H7: the GL forwarders and their drift test against `plx_gfx`'s imports, the shader table
against `gfx.rs` / `text.rs`, the state machine, the preflight and its refusals, the panel lines, the
bisect's variants and summary against a fake driver that rejects the way the emulator did and against a
model of the split hypothesis (the link variants' expected marks), the frozen original against the
repository's `vs_ambient.vert`, the one-multi-declarator-statement premise against every renderer shader,
the link log lines and summary, the stub read-back's labelling, the ES preamble against `gfx.rs`, the GL
state hand-over replayed on a model of GL's
state, the frame count and the settled report, the font wording, the log's no-truncation chunking, the
first-resume wording;
H8: the `mock-host.txt` parse, the line formats, the SONAME list against `net.rs`, a real libcurl GET
against a loopback server), the ARM link of `libplx.so` with and without `--no-gfx` and the checks above
(the same five `plx_tizen_*` exports, no `libSDL2` in `DT_NEEDED`, no undefined `gl*`, the 50 forwarders
exported, the git sha inside the object), `cargo check` for the host and for `x86_64-unknown-linux-gnu`
(with and without `gfx`), `bash -n build.sh`, `tests/mock_pms.py --host 127.0.0.1` answering `/identity`,
the `--tpk-only` refusal without certificates, and the ARM `.tpk` build with throwaway certificates (the C#
host compiles with the four-argument `plx_tizen_create`, 0 warnings, and the package lists
`res/appfont.ttf`, `res/OFL.txt` and the same `libplx.so`; that was before H7 and H8). Not verified:

- **H7 and H8 on any TV, and H7 past the preflight on any GL.** Whether all 14 `plx_gfx` shaders (written
  for the television's Mali) compile and link on YaGL with the split `vs_ambient.vert` (the bisect's `a`
  and `L1` say so; not yet run, and the fragment sources have only been compiled, never run), whether
  `init_blur` and `init_image` run there, what `glGetError` says, whether the panel still draws
  after the inits and the frames, where the font comes from, and whether libcurl exists on a Tizen TV under
  one of the three SONAMEs (it does on the emulator, as reported), are what these probes are for.
- **The split declaration on the television's own driver** (finding 5): the television compiled the
  unsplit source, and nothing has been run there with the split one; this branch does not change what the
  application ships. Whether the split is what YaGL needed is a hypothesis until a run's `a0`/`a`,
  `L3`/`L1` and `i0`/`i2` pairs say so, and the model of it in the unit tests is only a model.
- **The `x86_64` link with `plx_gfx`** (see "H7", "How the x86 link works") was reported clean on the
  emulator's build machine; on the author's Mac only `cargo check` passes for that target.
  `--no-gfx` is the fallback.
- That `__wrap_TTF_OpenFont` (the font retry) finds `res/` on a device: it uses the directory the host
  gave `plx_tizen_create`, the same one H5 uses; the log's `H7 gfx` lines say which font opened.
- The mock-server reachability from a real TV (the emulator inside WSL reached it at `10.0.2.2` with the
  mock on `127.0.0.1`, as reported; a TV needs a LAN address and `--host 0.0.0.0`, see "H8").

- That the panel is legible in a phone photo of a real TV (the host test reads the rendered panel at
  1872x1048 and the text is 28 px on a 1080-line screen; nobody has looked at it on a screen), and the
  whole Apps2Samsung path ("Tester build").
- That the C# host's facts arrive intact through the `LPStr` marshaling on a device (the create test
  feeds the same `name=value` lines from Rust).

- The `x86_64` and `i686` SDL builds and libraries (same script and code path as ARM apart from the
  compiler and `--host`; never run, there was no Linux machine). The WSL2 session is the first run.
- Everything on a device or the emulator: that the ARM `libplx.so` loads with the SDL objects inside it
  (an unresolved libc symbol shows only at `dlopen`; the build fails on any unversioned undefined
  symbol other than the known `getauxval`), that `SDL_Init` and its timer thread behave under the .NET
  host, the GLES2 upload and draw, and the packaged font reached through `DirectoryInfo.Resource`.
- That `DirectoryInfo.Resource` points at the package's `res/` on a device (the `H5 text ... res dir`
  line shows what the host passed).

- The network log on a device: that the host finds `res/log-host.txt` through `DirectoryInfo.Resource`
  (the build verifies the file is in the package, nothing more), that the `internet` privilege lets a
  UDP `sendto` through, and that a TV's network path reaches the receiver. On the Mac only: the sender
  against a loopback receiver (`cargo test` in `plx/`), the signing path with throwaway certificates,
  and that the C# host compiles.
- The `Ecore_Event_Key` layout. `keyname`, `key` and `keycode` are read from the upstream EFL header's
  offsets; Samsung's fork may differ, so the first events are also hex-dumped (6, was 5), and the four
  string pointers are read only after a per-page readability probe, so a wrong layout shows `UNREADABLE@0x...`
  in the log and `(none)` on the panel instead of crashing the app.
- Every Ecore/EGL symbol name and signature is copied from flutter-tizen's embedder, not checked
  against a device; the `sym ...` lines say what exists. `ECORE_WL2_WINDOW_TYPE_TOPLEVEL` = 1 is from
  memory of the EFL header.
- The `DllImport("plx")` spelling (nuvio's proven one is `"libnuvio.so"`; the host retries after a
  `dlopen` by absolute path and logs `dlerror`).
- `tizen80` on sets newer than 6.0 (nuvio ships `tizen90` for 6.5-7 and a different TFM for 8+).
- That ARM Tizen 6-9 sets are 32-bit ARMv7 armel. Some may be aarch64: the log says.
- The ARM library has one symbol left unresolved at link time, `getauxval` (glibc 2.16+, absent from
  the NDK's 2.12 sysroot, used by Rust's std). It binds to the TV's own glibc at load time. Its
  `DT_NEEDED` also lists `librt.so.1`, `libm.so.6` and `ld-linux.so.3` beside libc/libdl/libpthread/libgcc_s
  (`libm` since H5: SDL and FreeType use it).
