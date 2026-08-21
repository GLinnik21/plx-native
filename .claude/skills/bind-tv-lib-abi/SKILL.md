---
name: bind-tv-lib-abi
description: >
  Bind a new function or struct field from one of the TV's own native libraries —
  FFmpeg (libavformat / libavcodec / libavutil / libswscale), libcurl, or the C++
  StarfishMediaAPIs / ACB seam. Use when adding a demuxer, encoder, decoder, muxer,
  Starfish or ACB call; when adding a whole new stubbed `.so`; when a link fails with
  "undefined reference to av_*"; or when debugging garbage field values, a suspected
  wrong struct offset, or a SIGSEGV inside a TV library. The stub `.so` trick makes
  every link succeed whether or not the symbol exists on the device, so symbol
  presence and struct offsets must be PROVEN against the device's own binaries before
  the code is written — a wrong offset is silent memory corruption on a device with no
  debugger.
---

# Binding the TV's own libraries — prove it before you write it

We link against hand-written stub `.so` files carrying the TV's real SONAMEs (see
`CLAUDE.md`), so **the link always succeeds**. Nothing checks that a symbol exists on
the device, and nothing checks that a struct layout you took from an upstream header
matches the build the TV actually ships. Both fail only at runtime — as a wrong value,
or a SIGSEGV, on a device with no debugger.

This project has paid for that twice: `setHdrInfo` linked fine and was simply absent on
webOS 4.5 (feature dropped after the fact), and `docs/ffmpeg-demuxer-plan.md` ranks "ABI
offset mismatch" as the highest project risk. The FFmpeg struct offsets in `ff.rs` have
been re-derived from device binaries at least five separate times.

All paths below are relative to the repo root. The TV address comes from the `Makefile`
(override with `TV=…`); nothing here hardcodes a host or a credential.

## The driver

```bash
tools/abi-probe.sh info <lib>                  # SONAME, NEEDED, CPU arch, build strings
tools/abi-probe.sh has  <lib> <sym> [sym...]   # PRESENT/ABSENT per symbol (exit 1 if any absent)
tools/abi-probe.sh syms <lib> [regex]          # exported symbols, demangled
tools/abi-probe.sh opts <lib> [name-regex]     # FFmpeg: real struct offsets from AVOption tables
tools/abi-probe.sh pull <lib>                  # refresh the cached copy
```

`<lib>` is a SONAME (`libavcodec.so.57`) or a device path. First use pulls the library
into `.abi-cache/` (gitignored) and follows the SONAME symlink, so the cache records the
exact build. **The TV has no binutils** — everything runs host-side with the NDK's cross
tools, so `WEBOS_SDK` must be set up (the `setup-environment` skill).

## The procedure

### 0. Stub or real?

If the NDK sysroot already has the library, link it **real** (add `-l<name>` to
`LIBS_REAL`) and get genuine link-time checking. Only stub a library the sysroot lacks
or ships under a different SONAME than the TV. Today that is FFmpeg + libcurl only.

### 1. Prove the symbol exists ON THE DEVICE

```bash
tools/abi-probe.sh has libavcodec.so.57 avcodec_send_frame avcodec_receive_packet
```

Do not skip this because the code compiled — it always compiles. Do not trust upstream
headers or the sysroot: neither is the device. For the C++ seam, check the exact mangled
name as spelled in `src/starfish.c`'s `__asm__` declarations (`syms` demangles, so search
the demangled form and then use the mangled one).

### 2. Pin the build

`tools/abi-probe.sh info <lib>` prints the SONAME and version. Record it in a comment
next to the offsets you derive. Layouts differ between builds; an offset is only valid
for the build you read it from. This TV: FFmpeg n3.3 family (`libavcodec.so.57.89.100`,
`libavformat.so.57.71.100`, `libavutil.so.55.58.100`, `libswscale.so.4.6.100`), 32-bit
ARM EABI.

### 3. Get the field offset — in this order

**a. Prefer the library's own name-based API. No offset, no risk.** For FFmpeg that
means `av_opt_set` / `av_opt_set_int` for every numeric knob, and `av_get_pix_fmt` /
`avcodec_get_name` at runtime instead of hardcoded enum values. Enum values *shift
between builds*: `AV_PIX_FMT_RGBA` is **28** in this build, not the value you get by
counting a current header, because an `FF_API_XVMC` alias consumes slots.

**b. Read the real offsets out of the library.** FFmpeg embeds an `AVOption` table for
every option-backed field, and each record carries that field's true offset for *this*
build:

```bash
tools/abi-probe.sh opts libavcodec.so.57 '^(b|g|bf|time_base|bufsize|maxrate)$'
```

```
b                                 72  INT64      <- AVCodecContext.bit_rate
flags                             92  FLAGS
time_base                        108  RATIONAL
g                                140  INT        <- gop_size
bf                               160  INT        <- max_b_frames
bufsize                          512  INT
maxrate                          528  INT64
```

These libraries are stripped, so the driver *scans* the data sections for `AVOption`-
shaped records rather than looking up a table symbol. It scans **all** tables in the
library, so the same option name can appear more than once from different `AVClass`es —
cross-check a match against a neighbouring field whose offset you already trust.

**c. Only then, derive by hand from headers** — for fields with no AVOption (in
`AVCodecContext`: `width`, `height`, `pix_fmt`). Fetch the matching-version header, and
account for two things that bite on this target:
- **`FF_API_*` blocks are PRESENT here.** `libavcodec` major 57 < 58 means `codec_name[32]`
  and `stream_codec_tag` still occupy space; a layout transcribed from a 4.x header
  misplaces every field after them.
- **32-bit ARM aligns `int64`/`double` to 8 bytes.** `AVFrame.pts` sits at +104 because a
  4-byte pad at +100 aligns it. Miss the pad and every later field is wrong.

Record each offset as a named constant with its derivation in the comment — the house
pattern is `ff.rs`'s `OFF_STREAM_*` / `OFF_CTX_*` / `OFF_FRAME_*` blocks.

### 4. Add the stub symbol

An empty body whose **name** matches, in the matching `stub/*_stub.c`. Signature doesn't
matter (`void foo(void){}` is fine) — only the name is resolved at link time.

**A whole new library** takes five steps (libswscale is the worked example):
1. Confirm the device's SONAME: `tools/abi-probe.sh info <lib>`.
2. `stub/<name>_stub.c` with the empty bodies.
3. A Makefile rule with `-Wl,-soname,<the exact device SONAME>`.
4. Add `-l<name>` to `LIBS_STUB`.
5. Add `stub/lib<name>.so` to `STUBS`.

### 5. Add a runtime ABI self-check

Any code that pokes raw offsets must prove them at runtime and **disable itself** on
mismatch rather than run on. The pattern (`ff.rs`'s `Venc::open`): after opening the
codec, round-trip the poked fields through an already-trusted struct
(`avcodec_parameters_from_context`) and compare. On mismatch, log and latch the feature
off. Corrupting memory silently is far worse than losing a dev feature.

### 6. Verify on-device

Build, deploy, and read the event log — `make -s print-eventlog FLAVOR=<f>`, since a flavoured
install writes it under `/tmp/<app id>` rather than `/tmp` (the `tv-session` skill drives all of
this, including the one-time `make FLAVOR=<f> install` a deploy cannot do for you).
If it SIGSEGVs, hand off to the `crash-triage` skill — a PC inside a TV `.so` with a bad
argument is the signature of exactly this class of bug.

## Gotchas

- **`AVOptionType` is not a 0..N enum in FFmpeg 3.x.** `CONST` is **128** and the richer
  types are four-char `MKBETAG` codes (`SIZE`, `PFMT`, `CHLA`…). Numbering it
  sequentially — the shape a 4.x header suggests — rejects every real record and accepts
  garbage. The driver has the correct map; if you write your own parser, copy it.
- **Dynamic symbols carry a version suffix** (`avcodec_send_frame@@LIBAVCODEC_57`). Strip
  at `@` before comparing, or every lookup reports ABSENT.
- **Encoder/decoder tables are not exported symbols.** "Does this build have an MPEG-1
  encoder?" can only be answered at runtime via `avcodec_find_encoder_by_name` — not by
  `nm`. (This build does keep encoders: mjpeg, mpeg1video, mpeg2video, mpeg4, h263p, png.)
- **The sysroot is not the device.** The NDK sysroot ships its own copies of some
  libraries at different versions than the TV runs. Probe the device.
- **NEON is not a given.** The build flags disable NEON for Rust codegen, and the TV's own
  `libjpeg.so.62` claims libjpeg-turbo but contains zero NEON instructions. If you are
  choosing a conversion path for performance, check with
  `$(WEBOS_SDK)/bin/*-objdump -d <lib> | grep -c 'vld1\|vmull'` before assuming.
- **Existing deep-dives own their domains.** For Starfish/ACB bind order, the `uid=NULL`
  rule, the sret `std::string` convention and the 3-arg taskId ABI, read
  `rust-modules/src/player/CLAUDE.md` — don't rediscover them here.

## Troubleshooting

| Symptom | Cause / fix |
|---|---|
| `undefined reference to av_*` at link | The symbol isn't in the matching `stub/*_stub.c`. Add an empty body — but run step 1 first: if it's ABSENT on the device, the stub only moves the failure to runtime. |
| `ERROR: NDK binutils not found` | `WEBOS_SDK` unset or the NDK isn't installed — see the `setup-environment` skill. |
| Field reads garbage / plausible-but-wrong values | Wrong offset. Re-derive via step 3b, check for an `FF_API_*` block and for `int64` alignment padding. |
| SIGSEGV inside a TV `.so` | Bad argument or wrong offset, not a bug in their library. `crash-triage` skill; then re-verify every offset the call path touches. |
| `opts` prints nothing | Wrong library for that option, or the name is a codec-private option in a different `AVClass`. Widen the regex to `.` and look for the neighbourhood. |
