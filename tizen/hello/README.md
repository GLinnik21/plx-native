# PlxNative on Samsung Tizen TV: step 1, "hello world"

A throwaway test build (branch `samsung/hello-tpk`, never merged). It is a `.tpk` that a 2021+
Samsung TV (Tizen 6.0+) or the Tizen TV emulator can install. A C# host loads our Rust `libplx.so`;
the library opens its own native window, clears it to a colour with EGL/GLES2, logs remote keys and
writes a log file.

It answers four questions:

1. **Does the host start?** Lines tagged `[host ...]` in the log.
2. **Does our .so load?** The `plx-hello 0.1.0` line (written by the .so).
3. **Does the window draw?** The screen turns **orange** and the log says `first frame presented`.
4. **Do keys arrive?** Every remote key is logged, and **each key press changes the colour**
   (orange, red, green, blue, yellow, magenta, cyan, white, back to orange). That works without
   reading any log.

Why this shape: retail Samsung TVs refuse a native `capp` package, so the package is `type="dotnet"`
and its C# host P/Invokes a native library (flutter-tizen's model). The host is a plain
`CoreUIApplication` whose `OnCreate` calls the library once on the main thread; the library creates
an Ecore_Wl2 window there (Ecore's main loop runs on that thread), registers key handlers there,
and a separate Rust thread owns the EGL context and draws.

## Layout

```
tizen/hello/
  plx/                  Rust crate -> libplx.so (own [workspace], only dependency: libc)
  host/PlxHello/        C# host + tizen-manifest.xml (package and app id: plxnhello0)
  build.sh              builds the .so and the .tpk
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
tizen/hello/build.sh                    # arm: out/arm/libplx.so, out/PlxHello-arm.tpk
tizen/hello/build.sh --arch x86_64      # in WSL2: out/PlxHello-x86_64.tpk
tizen/hello/build.sh --arch i686        # in WSL2, for the 9.0 emulator image
tizen/hello/build.sh --arch x86_64 --so-only    # no .NET (then --tpk-only elsewhere)
```

Prerequisites, all user-local, nothing system-wide:

- Rust via rustup (`arm` uses the repo's pinned toolchain, `rustup target add` is run for you).
- `arm` only: the webOS NDK at `~/webos-ndk/arm-webos-linux-gnueabi_sdk-buildroot` (`WEBOS_SDK` overrides).
- x86 on WSL2: `apt install build-essential unzip` (and `gcc-i686-linux-gnu` for `i686`).
- .NET 8 SDK in `~/.dotnet`: `dotnet-install.sh --channel 8.0 --install-dir ~/.dotnet`. The package
  uses the legacy `Tizen.NET.Sdk` NuGet route (`tizen80`, which covers Tizen 6.0), not the
  `net6.0-tizen` workload, which only accepts Tizen 8+. NuGet restore needs the internet.

`build.sh` verifies the library and the package after building (file type, ELF attributes,
DT_NEEDED, highest glibc symbol version, exports, `unzip -l`) and fails on a surprise. A library
needing a newer glibc than the target has (`PLX_MAX_GLIBC`) is a failure: on a WSL2 distro newer than
the emulator, see `X86_SYSROOT` in the script header.

The `.tpk` is signed with the SDK's default developer certificate. Re-sign it per TV with
Apps2Samsung (below). No certificate is ever committed.

## Install and run

**Real TV (Developer Mode on):** sideload `out/PlxHello-arm.tpk` with Apps2Samsung, which re-signs it
for the TV. Launch "PlxHello" from the TV's apps.

**Emulator:** the Tizen SDK 10.0 CLI installer plus `package-manager-cli` with **TV Extension 10.0**
(Tizen Studio, the IDE, is deprecated). Then, with the emulator running:

```sh
sdb install out/PlxHello-x86_64.tpk       # i686 for the 9.0 image
sdb shell app_launcher -s plxnhello0      # unverified command; or launch it from the app list
```

The emulator cannot test `Tizen.Multimedia` playback; this step does not need it.

## The log

The file is `<app data dir>/plx-hello.log`, appended and flushed per line, written by both the host
(`[host ...]`) and the library (`[plx +secs tTID]`). The data dir is the host's `DirectoryInfo.Data`
(its path is the first host line); typically `/opt/usr/home/owner/apps_rw/plxnhello0/data/`.

- Emulator: `sdb pull /opt/usr/home/owner/apps_rw/plxnhello0/data/plx-hello.log`, or
  `sdb shell cat` it. Host lines also go to dlog: `sdb dlog PLXHELLO`.
- Retail TV: there is no `sdb shell`. `sdb connect <tv-ip>` works in Developer Mode and
  `sdb pull` of that file may or may not be allowed (not verified). If it is not, report what you saw
  on screen (below); that alone answers questions 1 to 4.

## What to report

1. Did the app start (host) and did anything appear? Did the log get written?
2. Did the .so load? If not, paste the `[host ...]` lines (`DllNotFoundException`, the `preload dlopen
   ... FAILED:` text is the loader's own reason).
3. Did the window turn orange? Did it stay on screen?
4. Do keys arrive? Press arrows, OK, Back, colour buttons, media keys: does the colour change?
   Paste the `key ...` lines, the `key raw event` lines and any `window focus` lines.
5. These lines, always: `uname machine=`, `arch(compile-time)`, `glibc`, the `platform.version` and
   `model_name` host lines, `cpuinfo`, `EGL vendor=`, `GL vendor=/renderer=`, `screen size`.
6. If it failed: the `plx_tizen_create FAILED -> N (NAME)` line. Codes are in `plx/src/lib.rs`
   (`code`): 4 a library did not open, 5 a symbol is missing (the `MISSING (required)` lines name it),
   6 to 11 Ecore/window/key handlers, 12 to 16 EGL, 17 to 19 the render thread. Host-side: -1 .so not
   found, -2 export missing, -3 other.

## Unverified (nothing here has run on a device)

- The `Ecore_Event_Key` layout. `keyname`, `key` and `keycode` are read from the upstream EFL header's
  offsets; Samsung's fork may differ, so the first events are also hex-dumped.
- Every Ecore/EGL symbol name and signature is copied from flutter-tizen's embedder, not checked
  against a device; the `sym ...` lines say what exists. `ECORE_WL2_WINDOW_TYPE_TOPLEVEL` = 1 is from
  memory of the EFL header.
- The `DllImport("plx")` spelling (nuvio's proven one is `"libnuvio.so"`; the host retries after a
  `dlopen` by absolute path and logs `dlerror`).
- `tizen80` on sets newer than 6.0 (nuvio ships `tizen90` for 6.5-7 and a different TFM for 8+).
- That ARM Tizen 6-9 sets are 32-bit ARMv7 armel. Some may be aarch64: the log says.
- The ARM library has one symbol left unresolved at link time, `getauxval` (glibc 2.16+, absent from
  the NDK's 2.12 sysroot, used by Rust's std). It binds to the TV's own glibc at load time. Its
  `DT_NEEDED` also lists `librt.so.1` and `ld-linux.so.3` beside libc/libdl/libpthread/libgcc_s.
