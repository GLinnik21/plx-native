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
  build.sh              builds the .so and the .tpk (signed; --log-host adds the network log)
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
tizen/hello/build.sh --arch x86_64 --so-only    # no .NET, no certificates (then --tpk-only elsewhere)
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
7. If it failed: the `plx_tizen_create FAILED -> N (NAME)` line. Codes are in `plx/src/lib.rs`
   (`code`): 4 a library did not open, 5 a symbol is missing (the `MISSING (required)` lines name it),
   6 to 11 Ecore/window/key handlers, 12 to 16 EGL, 17 to 19 the render thread. Host-side: -1 .so not
   found, -2 export missing, -3 other.

## Unverified (the app has not run yet: the first emulator install stopped at signing)

- The network log on a device: that the host finds `res/log-host.txt` through `DirectoryInfo.Resource`
  (the build verifies the file is in the package, nothing more), that the `internet` privilege lets a
  UDP `sendto` through, and that a TV's network path reaches the receiver. On the Mac only: the sender
  against a loopback receiver (`cargo test` in `plx/`), the signing path with throwaway certificates,
  and that the C# host compiles.
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
