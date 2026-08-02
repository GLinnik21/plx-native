# Third-party notices

This file accompanies the **PlxNative** application package (`com.beb.plxnative`), an unofficial
native Plex client for LG webOS 4.x televisions. PlxNative itself is Copyright (c) 2026 Gleb
Linnik and is distributed under the MIT License (see `LICENSE` in the source distribution).

The file is organised by **relationship**, because relationship — not licence — is what
determines the obligation:

1. **Redistributed in this package** — third-party code or assets that are physically inside the
   files you received (compiled into the `plxnative` binary, embedded in it, or shipped beside
   it). These carry real attribution obligations, discharged here and in `licenses/`.
2. **Dynamically linked, not redistributed** — libraries that already exist on your television
   and are loaded at run time by SONAME. No copy of them is contained in this package.
3. **Not determined** — components we could not establish a licence for, listed as unknown
   rather than guessed.

Full licence texts are **not** repeated inline. They are in the `licenses/` directory of this
package; the exact required contents of that directory are listed in section 5.

---

## 1. LGPL-2.1 components, dynamic linking, and your right to relink

PlxNative links dynamically against the following **LGPL-2.1-or-later** libraries, all of which
are part of your television's own system software and are **not** distributed by us:

| Library | Version on this webOS 4.5 build | SONAME(s) the app requests |
|---|---|---|
| FFmpeg — libavformat, libavcodec, libavutil, libswscale | 3.3 (LG `lib32-ffmpeg/3.3-r0`) | `libavformat.so.57`, `libavcodec.so.57`, `libavutil.so.55`, `libswscale.so.4` |
| GLib | 2.48.2 | `libglib-2.0.so.0` |
| GNU C Library (glibc) | 2.24 | `libc.so.6`, `libm.so.6`, `libpthread.so.0`, `librt.so.1`, `libdl.so.2`, `ld-linux.so.3` |

**These libraries and their use are covered by the GNU Lesser General Public License, version
2.1 or later.** A complete copy of that licence is supplied with this package at
`licenses/LGPL-2.1.txt`.

**You may modify and replace them.** PlxNative uses the ordinary shared-library mechanism
(LGPL-2.1 §6(b)): no code from these libraries is copied into the `plxnative` executable — it
records the SONAMEs above as `DT_NEEDED` entries and the dynamic loader resolves them at run time
against the television's own `/usr/lib`. To use your own build, install an interface-compatible
library under the same SONAME so the loader finds it in place of the stock one; PlxNative will
load it with no change to the application. The MIT terms under which PlxNative is distributed
permit modification of the application for your own use and reverse engineering for debugging
such modifications.

Two disclosures, so this claim is not overstated:

- The application reads a small number of FFmpeg struct fields at byte offsets fixed for the ABI
  denoted by the SONAMEs above (`libavformat.so.57` / `libavcodec.so.57`). Replacements that are
  interface-compatible with those SONAMEs work; a rebuild that changes struct layout without
  bumping the SONAME would not.
- Small fragments of glibc's own startup and compatibility code **are** statically linked into
  `plxnative` by the toolchain (`crt1.o` and objects from `libc_nonshared.a` — this is why the
  binary defines `_start`, `__libc_csu_init`, `__libc_csu_fini`, `fstat64`, `lstat64`,
  `fstatat64`). They are covered by the same LGPL-2.1 notice and licence copy above. Whether
  those particular files additionally carry glibc's linking exception was **not verified** for
  the NDK build used here.

Because the shared-library mechanism of §6(b) is used, no source code or relinkable object code
is supplied under §6(a). FFmpeg's own request (ffmpeg.org/legal.html) that distributors point at
the corresponding source is honoured informally: the FFmpeg 3.3 sources are available from
ffmpeg.org; the build on this television is LG's, configured with `--disable-gpl`, without
`--enable-version3` and without `--enable-nonfree` (verified by reading the configure string out
of the device's own `libavutil.so.55`), so no GPL-licensed FFmpeg component is present.

---

## 2. Redistributed in this package

### 2.1 Vendored C source, compiled into `plxnative`

**nanosvg** and **nanosvgrast** — Copyright (c) 2013-14 Mikko Mononen <memon@inside.org>
Licence: **Zlib** (`licenses/Zlib.txt`). The vendored headers are byte-identical to upstream
(github.com/memononen/nanosvg); they are not altered.
The upstream headers credit, and we reproduce, the following derivations:

- The SVG parser is based on **Anti-Grain Geometry 2.4** SVG example — Copyright (C) 2002-2004
  Maxim Shemanarev (McSeem).
- Arc calculation code is from **canvg** (https://code.google.com/p/canvg/).
- Bounding-box calculation is based on the method described at blog.hackers-cafe.net.
- The polygon rasterizer is heavily based on the **stb_truetype** rasterizer by Sean Barrett
  (http://nothings.org/).

### 2.2 Icon artwork embedded in `plxnative`

The following SVG icons are compiled into the binary. All are visually modified from upstream
(stroke width and/or colour); modifications are stated where the licence requires it.

**Google Material Design Icons** — the `person` icon (`assets/icons/user.svg`)
Copyright Google LLC. Licence: **Apache License 2.0** (`licenses/Apache-2.0.txt`).
*Modified*: the transparent bounding-box path was removed and an explicit white fill added.
The upstream repository contains no `NOTICE` file, so no NOTICE content is propagated.

**Feather Icons** — the `delete` icon (`assets/icons/backspace.svg`)
Copyright (c) 2013-2023 Cole Bemis. Licence: **MIT** (`licenses/MIT.txt`).
*Modified*: stroke width 2 → 2.2, stroke colour set to white.
The three chevron icons (`chevron.svg`, `chevron-down.svg`, `chevron-up.svg`) use the same vertex
coordinates as Feather's `chevron-right` / `chevron-down` / `chevron-up`, re-expressed as paths
at stroke width 3. Whether that constitutes derivation or independent authorship of trivial
geometry was **not established**; they are credited here out of caution under the same notice.

**Heroicons** — the `check` icon (`assets/icons/check.svg`)
Copyright (c) Tailwind Labs, Inc. Licence: **MIT** (`licenses/MIT.txt`).
The path data `M5 13l4 4L19 7` on a 24×24 grid is identical to Heroicons v1
`optimized/outline/check.svg` (confirmed against upstream). *Modified*: stroke width 2 → 3,
stroke colour set to white.

The remaining icons in the application are original work by the PlxNative author. See section 4
for a trademark note that applies to some of them.

### 2.3 Fonts

**Inter** — `appfont.ttf`, `appfont-bold.ttf`
Copyright 2016 The Inter Project Authors (https://github.com/rsms/inter), per the fonts' own
name table; the licence file shipped with this package states "Copyright 2020". Version
4.001 (git-66647c0bb).
Licence: **SIL Open Font License 1.1** — the full text ships in this package as `OFL.txt`.
*Modified*: these are static instances cut from the Inter variable font at weight 400 / 700 and
optical size 18, with tabular figures frozen and a legacy `kern` table synthesised. Inter's
copyright statement declares **no Reserved Font Name**, so OFL §3 imposes no rename and the
family name "Inter" is retained. The fonts also carry the upstream trademark statement
"Inter UI and Inter is a trademark of rsms."

### 2.4 Rust code statically linked into `plxnative`

The application core is Rust. The Rust standard library is compiled from source
(`-Z build-std`) and linked in, together with the crates below. All of this code is
redistributed in the binary. Where a package offers a choice of licences, our election is
stated; election does not alter your rights under the other arms.

**Elected MIT** (`licenses/MIT.txt`), with the copyright holders required by that licence:

| Package | Version(s) | Declared licence | Copyright |
|---|---|---|---|
| The Rust standard library (`core`, `alloc`, `std`, `panic_unwind`, `unwind`, and the `rustc-std-workspace-*` shims) | rustc 1.98.0-nightly (c397dae80 2026-07-02) | MIT OR Apache-2.0 | The Rust Project Developers |
| — its in-tree backtrace support (`library/backtrace`) | 0.3.76 | MIT OR Apache-2.0 | 2014 Alex Crichton |
| — its in-tree mpmc channels (`library/std/src/sync/mpmc`) | in-tree | MIT OR Apache-2.0 | 2019 The Crossbeam Project Developers |
| addr2line | 0.25.1 | Apache-2.0 OR MIT | The `addr2line` authors |
| adler2 | 2.0.1 | 0BSD OR MIT OR Apache-2.0 | Jonas Schievink, oyvindln |
| bitflags | 2.13.0 | MIT OR Apache-2.0 | 2014 The Rust Project Developers |
| bytemuck | 1.25.0 | Zlib OR Apache-2.0 OR MIT | 2019 Daniel "Lokathor" Gee |
| byteorder-lite | 0.1.0 | Unlicense OR MIT | 2015 Andrew Gallant |
| cfg-if | 1.0.4 | MIT OR Apache-2.0 | 2014 Alex Crichton |
| crc32fast | 1.5.0 | MIT OR Apache-2.0 | 2018 Sam Rijs, Alex Crichton and contributors |
| fdeflate | 0.3.7 | MIT OR Apache-2.0 | The image-rs Developers |
| flate2 | 1.1.9 | MIT OR Apache-2.0 | 2014-2026 Alex Crichton |
| gimli | 0.32.3 | MIT OR Apache-2.0 | The `gimli` authors |
| hashbrown | 0.17.1 | MIT OR Apache-2.0 | Amanieu d'Antras |
| image | 0.25.10 | MIT OR Apache-2.0 | The image-rs Developers |
| itoa | 1.0.18 | MIT OR Apache-2.0 | David Tolnay |
| libc | 0.2.186 and 0.2.185 | MIT OR Apache-2.0 | The Rust Project Developers |
| memchr | 2.8.2 and 2.7.6 | Unlicense OR MIT | 2015 Andrew Gallant |
| miniz_oxide | 0.8.9 | MIT OR Zlib OR Apache-2.0 | 2013-2014 RAD Game Tools and Valve Software; 2010-2014 Rich Geldreich and Tenacious Software LLC; (c) 2017 Frommi; (c) 2017-2024 oyvindln |
| num-traits | 0.2.19 | MIT OR Apache-2.0 | 2014 The Rust Project Developers |
| object | 0.37.3 | Apache-2.0 OR MIT | The `object` authors |
| png | 0.18.1 | MIT OR Apache-2.0 | 2015 nwin |
| rustc-demangle | 0.1.27 | MIT/Apache-2.0 | Alex Crichton |
| serde | 1.0.228 | MIT OR Apache-2.0 | Erick Tryzelaar, David Tolnay |
| serde_core | 1.0.228 | MIT OR Apache-2.0 | Erick Tryzelaar, David Tolnay |
| serde_json | 1.0.150 | MIT OR Apache-2.0 | Erick Tryzelaar, David Tolnay |
| zune-core | 0.5.1 | MIT OR Apache-2.0 OR Zlib | The zune-image developers |
| zune-jpeg | 0.5.15 | MIT OR Apache-2.0 OR Zlib | The zune-image developers |

**MIT only — no alternative arm** (`licenses/MIT.txt`):

| Package | Version | Copyright |
|---|---|---|
| simd-adler32 | 0.3.9 | (c) 2021 Marvin Countryman |
| zmij | 1.0.21 | David Tolnay (a Rust port of Victor Zverovich's C++ `zmij`) |
| rust-lang/libm — compiled *inside* `compiler_builtins` and exposed as intrinsics on this target | as vendored in rustc 1.98.0-nightly | (c) 2018 Jorge Aparicio; musl libc (c) 2005-2020 Rich Felker, et al.; CORE-MATH; and the fdlibm-derived notice: (c) 1993, 2004 Sun Microsystems; (c) 2003-2011 David Schultz; (c) 2003-2009 Steven G. Kargl; (c) 2003-2009 Bruce D. Evans; (c) 2008 Stephen L. Moshier; (c) 2017-2018 Arm Limited |

**Elected Apache-2.0** (`licenses/Apache-2.0.txt`) — these two offer no MIT arm:

| Package | Version | Declared licence | Copyright |
|---|---|---|---|
| moxcms | 0.8.1 | BSD-3-Clause OR Apache-2.0 | (c) Radzivon Bartoshyk |
| pxfm | 0.1.29 | BSD-3-Clause OR Apache-2.0 | (c) Radzivon Bartoshyk |

**Conjunctive licence — no election possible:**

**compiler_builtins 0.1.160**, declared `MIT AND Apache-2.0 WITH LLVM-exception AND (MIT OR
Apache-2.0)`. Both `licenses/MIT.txt` and `licenses/Apache-2.0.txt` together with
`licenses/LLVM-exception.txt` apply. It contains code derived from **LLVM's compiler-rt**
(https://llvm.org/): work derived from compiler-rt prior to 2019-01-19 is used under the MIT
licence with the copyright "Copyright (c) 2009-2016 by the contributors listed in CREDITS.TXT"
(https://github.com/llvm/llvm-project/blob/main/compiler-rt/CREDITS.TXT); work derived after
that date is used under Apache-2.0 with the LLVM exception. The LLVM exception waives Apache-2.0
§4(a), (b) and (d) for portions embedded into object form by compilation; it waives nothing in
MIT, so the MIT notice above is required and is given.

**Unicode Character Database tables in Rust `core`**
(`library/core/src/unicode`, statically linked; reached by character classification and case
mapping): Copyright © 1991-2024 Unicode, Inc. Licence: **UNICODE LICENSE V3**
(`licenses/Unicode-3.0.txt`). That licence permits this notice to appear in associated
documentation, which is what this file is.

**Independent JPEG Group acknowledgement.** The `image` crate contains a Rust translation of
`jfdctint.c` from the Independent JPEG Group's libjpeg version 9a
(`src/codecs/jpeg/transform.rs`), reached through the JPEG encoder used by the application's
capture module, which is compiled into every configuration of the binary. As required by IJG
condition (2) for distribution of executable code:

> This software is based in part on the work of the Independent JPEG Group.

IJG code is copyright (C) 1991-2014, Thomas G. Lane, Guido Vollbeding.

**Not in the binary, listed to prevent a false conclusion.** `serde_derive`, `proc-macro2`,
`quote`, `syn`, `unicode-ident` and `autocfg` are host build-time machinery and contribute no
code to the shipped binary. `foldhash` appears in the Rust source tree but is **not** compiled
here (the standard library takes `hashbrown` with default features disabled, which does not
enable it) — verified absent from the binary. `rustc-literal-escaper`, `proc_macro`,
`panic_abort` and `std_detect` are compiled by `-Z build-std` but leave no code in this binary.

### 2.5 Compiler runtime fragments statically linked into `plxnative`

- **GCC runtime startup objects** (`crtbegin.o`, `crtend.o`) from the webOS NDK's GCC 12.2.0.
  Licence: GPL-3.0-or-later **WITH GCC-exception-3.1**. The GCC Runtime Library Exception
  permits distributing the result of compilation under our own terms, and the compilation used
  only Eligible Compilation Processes; no additional licence text is required or supplied.
- **`libglibc_polyfills.a`** from the webosbrew native-toolchain NDK (supplies `getauxval` and
  its initialiser). **Licence not determined** — see section 3.

---

## 3. Dynamically linked, not redistributed

The libraries below are part of your television's software. This package contains no copy of
them; PlxNative loads them at run time. Where a licence would require reproducing a notice in
copies, no copy is being distributed, so nothing is owed — the credits are given because they
are due in substance.

| Library | Version on this build | Licence | Note |
|---|---|---|---|
| FFmpeg (libavformat / libavcodec / libavutil / libswscale) | 3.3 | LGPL-2.1-or-later | See section 1 |
| GLib | 2.48.2 | LGPL-2.1-or-later | See section 1 |
| GNU C Library | 2.24 | LGPL-2.1-or-later | See section 1 |
| SDL2 (LG fork) | 2.0.4 | Zlib | Copyright (C) 1997-2016 Sam Lantinga |
| SDL2_ttf | 2.0.14 | Zlib | Zlib-licensed since 2.0.11 |
| libcurl | 7.53.1 (LG SONAME `libcurl.so.5`) | curl (MIT/X derivate) | Copyright (c) 1996 - 2017, Daniel Stenberg, <daniel@haxx.se>, and many contributors |
| libwayland-client | 0.3.0 | MIT | Copyright © 2008-2012 Kristian Høgsberg; © 2010-2012 Intel Corporation; © 2011 Benjamin Franzke; © 2012 Collabora, Ltd. The licence of *this LG build specifically* was not read off the device |
| luna-service2 | 3.21.2 | Apache-2.0 | Licence taken from the webOS OSE upstream project; not read off this LG build |
| libgcc_s | the television's own | GPL-3.0-or-later WITH GCC-exception-3.1 | Version not determined; its exported symbol versions stop at `GCC_4.7.0`. No obligation arises — the Runtime Library Exception covers it |
| FreeType | libtool 6.16.0, i.e. release 2.9.0 (inferred from the so-version, not read from the binary) | FTL OR GPL-2.0-or-later | **Not** linked by PlxNative — reached only inside the television's own SDL2_ttf. Credit given voluntarily: *Portions of this software are copyright © The FreeType Project (www.freetype.org). All rights reserved.* |

---

## 4. Not determined, and matters outside licensing

**Licence not established.** The following are used but we could not determine a licence, and we
decline to guess one:

- `libGLESv2.so.2` — the television's OpenGL ES 2.0 implementation (an LG shim over the ARM Mali
  driver). Proprietary; no published licence located.
- `libAcbAPI.so.1`, `libplayerAPIs.so.1` (StarfishMediaAPIs), `libpf-1.0.so.1` — LG proprietary
  media components of webOS. No published licence located. PlxNative calls their published ABI;
  nothing of LG's is copied into or distributed with this package.
- `libglibc_polyfills.a` from the webosbrew native-toolchain NDK, statically linked (see 2.5).
  No licence statement was found in the NDK tree or in the archive itself.

**Non-affiliation.** PlxNative is an independent, unofficial application. It is not affiliated
with, endorsed by, or sponsored by LG Electronics or by Plex GmbH. "webOS", "LG", "Plex" and all
other trademarks are the property of their respective owners.

**Trademarks in shipped artwork (not a licence matter).** Several original icons drawn for this
application depict third-party brand marks — Rotten Tomatoes / Fandango (the tomato and popcorn
marks), The Movie Database, and IMDb (reproduced as name-and-brand-colour chips, not as logos). The drawings are
original geometry, so no third-party copyright licence applies and nothing is owed under any
licence in this file; the underlying trademark questions are separate and are not resolved here.

---

## 5. The `licenses/` directory

This package must contain exactly the following licence texts, verbatim:

| File | Required by |
|---|---|
| `licenses/LGPL-2.1.txt` | FFmpeg, GLib, GNU C Library (§1) — GNU Lesser General Public License, version 2.1 |
| `licenses/MIT.txt` | Feather Icons, Heroicons, and the MIT-elected Rust packages (§2.2, §2.4). One copy of the MIT text; the copyright holders it refers to are the ones tabulated in this file |
| `licenses/Apache-2.0.txt` | Google Material Design Icons; moxcms; pxfm; compiler_builtins (§2.2, §2.4) |
| `licenses/LLVM-exception.txt` | compiler_builtins (§2.4) |
| `licenses/Unicode-3.0.txt` | Unicode Character Database tables in Rust `core` (§2.4) — UNICODE LICENSE V3, "Copyright © 1991-2024 Unicode, Inc." |
| `licenses/Zlib.txt` | nanosvg (§2.1) |

`OFL.txt` (SIL Open Font License 1.1, for Inter) already ships at the root of this package. Keep
it there; do not add a second copy under `licenses/`.

No BSD-3-Clause text is required because Apache-2.0 is elected for `moxcms` and `pxfm`. No
GPL-3.0 text is required: the only GPL-3-covered components are GCC runtime pieces carried by
the Runtime Library Exception and the television's own `libgcc_s`.

---

## 6. Deliberately not listed

Two third-party components exist in the PlxNative source repository but are **not** part of this
package, and therefore carry no obligation discharged here: **libjpeg-turbo**
(`libturbojpeg.so.0`, copied to developer televisions by the development deploy step only) and
**jsmpeg** (a host-side development tool). Neither is present in the installed application.
