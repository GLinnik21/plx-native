# The webOS 5+ port

**Status, 2026-08-09: TESTED ON WEBOS 6 AND 10. IT RUNS; PLAYBACK DOES NOT.**

mariotaku ran it on both while reviewing the Homebrew Channel submission
([apps-repo#224](https://github.com/webosbrew/apps-repo/pull/224)) and marked it draft. His first
report read as a crash; the correction an hour later is the useful one:

> It didn't crash and I was able to see the media library with 0.2.0. But I didn't get media to
> load. Not necessarily incompatible (can't prove), but it stuck in buffering state.

So on 5+ the process starts, SDL/GLES/wayland work, sign-in works, the data layer works and the UI
renders. The failure is confined to playback — which is exactly and only the part that had never
run on hardware. The bound stays `>=4.0`: this is an unfinished feature on a working app, and the
submission is a draft that cannot land until playback works anyway.

That result does not invalidate the static work below; it invalidates the inference drawn from
it. `tools/fwcompat.py` and webosbrew's own check both say the process starts and every symbol
resolves on 4.4.2 through 11.2.0, and both were answering "would the dynamic loader accept this
binary". Something after the loader is what fails. §4 was explicit that starting is not playing —
what it did not anticipate is that the app would not reach a UI at all.

WHAT IS NEEDED NEXT is `/tmp/plxnative-events.log` and `/tmp/plxnative-crash.log` from one of those
sets. The first says how far boot got and which video-plane path was chosen; the second is
append-only and survives the relaunch, so it holds the faulting PC if the tracer ran. An empty or
absent events log means the loader killed the process before `main`, which would contradict the
static analysis and be the most interesting outcome of the two.

That distinction is the whole of this document. Everything below is either a fact provable on a
desk — and then it is stated flatly, with how to re-derive it — or an assumption that needs one
person with a 2020-or-later LG television to settle. The two are never mixed.

Written 2026-08-05. The research behind it is in this file; the code is on branch `webos5`.

---

## 1. What actually breaks, and how it presents

Before this work, the binary did not reach `main()` on anything past webOS 4.x. Not "playback
failed" — the dynamic loader refused to start the process, before the event log was open. To an
owner that is not a bug report, it is *nothing happens*.

Six `DT_NEEDED` entries caused it. A `DT_NEEDED` entry is a hard requirement for one exact SONAME,
and it cannot express "57 or 58":

| Library | webOS ≤4.10.0 | 5.3.1 | 7.4.0+ |
| --- | --- | --- | --- |
| `libAcbAPI` | `.so.1` | **deleted** | deleted |
| `libavformat` / `libavcodec` | `.so.57` | `.so.58` | `.58` → `.59` → `.60` |
| `libavutil` | `.so.55` | `.so.56` | `.56` → `.57` → `.58` |
| `libswscale` | `.so.4` | `.so.5` | `.5` → `.6` → `.7` |
| `libcurl` | `.so.5` | `.so.5` *and* `.so.4` | `.so.4` only |

Two of those rows correct things this repo previously believed. **libcurl does not break at
webOS 5** — 5.3.1 and 6.4.0 keep a `libcurl.so.5` compat alias beside the real `libcurl.so.4`, so
the break is at 7.4.0. And **webOS 3.9.2 has libAcbAPI**; `docs/distribution.md` §3.2 said it did
not. 3.9.2 fails for a different reason (FFmpeg 55, and a pre-C++11 `std::string` ABI, so
`StarfishMediaAPIs::Feed` carries a different mangling).

### An external check on the FFmpeg decision

webosbrew publishes guidance on this exact question, and it is a warning rather than a table —
<https://www.webosbrew.org/develop/caniuse/?q=ffmpeg>:

> Don't use system FFmpeg libraries! They will cause linkage issues and doesn't come with usable
> video codecs either.

Both clauses are the reasoning in §3.2 and §1: the SONAME moves with the firmware, and the
component list cannot be inspected from outside. The bundling decision here was reached
independently and then found to match; it is not a novel position, and re-litigating it means
disagreeing with the platform's own documentation.

Worth knowing where that page's data comes from, too: it is generated from
`WEBOSBREW_DEV_TOOLBOX_DATA` — the same dev-toolbox firmware database `tools/fwcompat.py` reads.
The site and the local matrix are the same facts, not two opinions.

## 2. The verification substrate — why any of this is checkable

`webosbrew-toolbox-fw-symbols` is not a checker. It is an **inventory**: for 14 real LG firmware
images, every library, its `DT_NEEDED`, and its complete exported-symbol list, keyed by webOS
release. 317 MB, entirely offline.

That is the substitute for hardware for every question of the form *"does this exist on webOS 5,
and will the binary load?"* — which turns out to be most of the port. `tools/fwcompat.py` reads it
directly and runs on macOS:

```sh
tools/fwcompat.py                                   # grade pkg/plxnative on all 14 releases
tools/fwcompat.py --release 5.3.1                   # one release, with the full missing list
tools/fwcompat.py --inventory libAcbAPI libavformat  # which releases carry these
tools/fwcompat.py --lib libSDL2-2.0.so.0 --grep webOS
```

It reproduces `webosbrew-ipk-verify`'s published verdict exactly (OK on 4.4.2 and 4.10.0 before
this work, FAIL on the other twelve), which is what makes it trustworthy when the numbers move.

**What it cannot tell you:** anything about behaviour. A firmware can export every ACB entry point
and still refuse to put a picture on the video plane. It grades whether the process *starts*.

### Firmware → release map

The database directories are firmware version strings, not webOS releases, and the two look alike
enough to mislead. `05.40.20.01` is webOS **4.10.0**, not 5. The mapping is in each directory's
`info.json`:

| release | firmware dir | year/chassis | FFmpeg | ACB | SDL |
| --- | --- | --- | --- | --- | --- |
| 3.9.2 | `06.19.80.01-W17H` | 2017 | 55 | ✅ | 2.0.4 |
| 4.4.2 | `05.50.15.01-W18R` | 2018 | 57 | ✅ | 2.0.4 |
| **4.10.0** | `05.40.20.01-W19P` | 2019 | **57** | ✅ | **2.0.5** ← the dev TV |
| **5.3.1** | `04.30.90.01-W20L` | 2020 | **58** (n4.0) | ❌ | 2.0.10 |
| 6.4.0 | `03.40.82.01-W21P` | 2021 | 58 (n4.0) | ❌ | 2.0.10 |
| 7.4.0 | `04.40.90.01-W22O` | 2022 | 58 (n4.2) | ❌ | 2.0.14 |
| 8.3.0 | `03.30.60.01-W23O` | — | 58 (n4.2) | ❌ | 2.0.14 |
| 9.2.0 | `23.20.50.01-W23O` | — | 58 (n4.4) | ❌ | 2.0.14 |
| 10.2.0 | `33.20.40.01-W22H` | — | 59 | ❌ | 2.0.14 |
| 11.2.0 | `43.21.70.01-W23O` | — | 60 | ❌ | 2.0.14 |

---

## 3. What was done

### 3.1 Runtime library binding (`rust-modules/src/dynlib.rs`)

FFmpeg, libcurl and libAcbAPI are `dlopen`'d by SONAME candidate list instead of linked. The
`dynlib!` macro takes a block shaped exactly like the `extern "C"` block it replaces, so no call
site moved. Loading is all-or-nothing: every symbol resolves, or the table stays empty and the
caller is told which symbols were missing.

**The stub `.so` trick is retired and `stub/` is deleted.** It existed to name libraries the
sysroot lacked, and in doing so pinned the binary to one firmware era — which was the entire
portability problem.

Measured effect, `tools/fwcompat.py`: **OK on 2 releases before, OK on 9 after.**

Two consequences worth knowing:

- The `cfg(test)` gate on `ff.rs`'s `#[link]` directives went with them. Host tests link
  unconditionally now; a call into FFmpeg fails by taking `dlopen`'s `None` branch on Darwin.
- `avformat_version` moved from the avutil block to the avformat block. Under `#[link]` the
  grouping was cosmetic because the final link resolved every name at once; `dlsym` searches one
  handle and its dependency chain, and libavutil does not depend on libavformat.

### 3.2 One FFmpeg ABI table per major (`ff.rs`'s `Abi`)

Eleven constants move between n3.3 and 4.x, and every one has a cause in FFmpeg's own source:

| | n3.3 (webOS ≤4.10) | n4.x (webOS 5.3.1–9.2.0) | why |
| --- | --- | --- | --- |
| `AVStream.time_base` | 40 | **16** | 4.0 deleted a deprecated `AVFraction pts` |
| `AVStream.codecpar` | 708 | **176** | same |
| `AVFormatContext.duration` | 1064 | **1072** | 4.0 inserted `char *url` at +1056 |
| `AVCodecContext.width/height/pix_fmt` | 124/128/144 | **92/96/112** | — |
| `AV_CODEC_ID_H264` | 28 | **27** | `FF_API_XVMC` removed |
| `AV_CODEC_ID_HEVC` | 174 | **173** | same |
| `AV_CODEC_ID_EAC3` | 0x15029 | **0x15028** | `FF_API_VOXWARE` removed |

(FFmpeg 9, which the app now ships, moves them again — HEVC is **172** there, `AVFrame.pts` is
**96** and `AVCodecParameters` drops the deprecated `channels` field entirely. That every major
shifts something is the argument, not a footnote to it.)

Everything else is untouched, and that is a finding rather than luck: the whole of `AVPacket`,
`AVCodecParameters`, `AVSubtitle`, `AVSubtitleRect` and `AVFrame` is byte-identical from 3.3 to
4.4, because the two deprecation guards that set those layouts
(`FF_API_CONVERGENCE_DURATION`, `FF_API_AVPICTURE`) are `MAJOR < 59` and survive all of FFmpeg 4.x.
**One table covers 58.12, 58.29 and 58.76** — webOS 5.3.1 through 9.2.0.

**SUPERSEDED — the app now bundles its own FFmpeg** (9.0, `ci/build-ffmpeg.sh`), so there is one
version instead of a table per firmware, and `ci/ffabi-assert.c` checks it against the very
headers the shipped libraries were built from. The per-major analysis above is kept because it is
the reason bundling won: offsets could be re-derived from a version string, but the *component*
list — which demuxers and bitstream filters LG compiled in — could not be checked at all.

Three sizes DO move within major 58 — `sizeof(AVStream)` is 688/704/**424** (4.4 moved 39 tail
fields into `AVStreamInternal`) — and none is in the table, because the app allocates none of those
structs and reads nothing in `AVStream` past `codecpar`.

### 3.3 The video-plane binding (`src/starfish.c`)

webOS 5 replaced ACB with a Wayland **exported window**, surfaced through LG's SDL fork. The app
asks the compositor for one, gets back a string id (`_Window_Id_<n>`), and puts that id into the
Starfish Load payload as `option.windowId`. The pipeline imports the window by name and punches
through to it — inside our own process, since `libplayerAPIs` pulls `liblsm-connector` transitively
on 5.3.1+ (`libplayerAPIs.so.1` → `libav-proxy.so.1` → `libav-connector.so.1` →
`liblsm-connector.so.1` → `libwayland-webos-client.so.1`).

The five entry points, from LG's own header — already present at
`$WEBOS_SDK/arm-webos-linux-gnueabi/sysroot/usr/include/SDL2/SDL_webOS.h`:

```c
const char *SDL_webOSCreateExportedWindow(int type);
SDL_bool    SDL_webOSSetExportedWindow(const char *windowId, SDL_Rect *src, SDL_Rect *dst);
SDL_bool    SDL_webOSExportedSetCropRegion(const char *windowId, SDL_Rect *org, SDL_Rect *src, SDL_Rect *dst);
SDL_bool    SDL_webOSExportedSetProperty(const char *windowId, const char *name, const char *value);
void        SDL_webOSDestroyExportedWindow(const char *windowId);
```

The window-type constants are spelled **`EXPORED`**, not `EXPORTED`, in the real header — LG's
typo. webosbrew's developer guide spells them correctly, which would not compile. `starfish.c` uses
the literal `0` and says why.

**What webOS 5 keeps:** all 15 mangled `StarfishMediaAPIs` / `CustomPipeline` symbols, unchanged,
through 11.2.0. The BUFFERSTREAM payload is identical but for that one key. So the engine, the
pump, the feed loop, seek and `pushEOS` are untouched.

**What it deletes, with no replacement:** `setSinkType`, `setMediaId`,
`setMediaVideoData(<sourceInfo>)` and the `LOADED`/`PLAYING`/`PAUSED` mirroring. Both reference
implementations agree — ss4s stubs them all to `return true`, Kodi guards each with `if (acb)`.
All that remains is placing the rect.

`vp_mode()` resolves which era this television is, once, by trying to `dlopen`/`dlsym` each.
Neither is linked in either direction: naming `libAcbAPI` kills the process on webOS 5, and naming
`SDL_webOSCreateExportedWindow` would do the same on webOS 4.5.

### 3.4 The SDL version handshake (`system.rs`) — a silent failure, now fixed

`sys_grab_wayland` declared SDL **2.0.4** to `SDL_GetWindowWMInfo`. From 2.0.6, SDL rejects a
caller claiming to be older than that and returns *without filling the union*. That leaves
`wl_surface` null, so `clear_opaque_region` silently no-ops, so the UI plane stays opaque — and
video decodes correctly and **invisibly** beneath it. Black screen, working audio, clean log.

It asks `SDL_GetVersion` now, and logs loudly if it ends up with no surface.

Which immediately found something: **the dev TV runs SDL 2.0.5, not 2.0.4.** That literal had been
wrong for the whole life of the project. It sat below the 2.0.6 guard, so it worked, and nothing
could have revealed it except asking.

---

## 4. What is NOT verified, and what would settle it

> **2026-08-11 — item 1 is SETTLED, in the affirmative.** The webosbrew reviewer ran v0.2.1 on a
> real LG 65UP7560AUD (webOS **6.5.2**, `Rockhopper 6.5.2-43`):
> [issue #22](https://github.com/GLinnik21/plx-native/issues/22). The whole `VP_EXPORTED`
> sequence ran — created, `windowId` spliced into the Load payload, placed `rv=1`, VDEC/ADEC
> allocated, fed to EOS with clean teardown. Six of eight playbacks worked: H.264 1080p, H.264
> 4K, HEVC, AAC and AC3, episodes, end-of-file. The two failures were server-side and
> firmware-independent (an HEVC-only transcode target on a server without Plex Pass — fixed the
> same day). Items 4 and 5 are implicitly settled by the same report — the reviewer browsed,
> navigated and played with the UI composited over live video. The `UNTESTED PATH` log banners
> are gone. Note the verification is one firmware (6.5.2): 5.x and 10/11 remain start-verified
> only, and item 3's in-place seek stays disabled on `VP_EXPORTED`.

Ranked by how much damage a wrong assumption does. *(Item 1 settled — see above.)*

1. **Does a picture appear on webOS 5?** The entire `VP_EXPORTED` path. Symbols proven present,
   call shapes taken from the two implementations that ship. Nothing else is known.
   *Settled by:* one person, one webOS 5+ TV, one play attempt, and
   `/tmp/plxnative-events.log`. Look for `vplane: SDL exported window`, then
   `vplane: exported windowId=_Window_Id_…`, then `vplane: exported window placed rv=1`.
2. **Was LG's libavformat 58 built from pristine sources?** The ABI table rests on FFmpeg's
   public-header invariant — layout is a function of the version macros alone, there is not one
   `#if CONFIG_*` in the public headers — plus an exact six-library version-triple match against
   upstream n4.0. Very strong, but a vendor *can* patch a public struct and bump nothing.
   *Settled by:* `/tmp/plxnative-ffprobe` with a known file on a webOS 5 set; if codec_id, width,
   height and time_base come back sane, the table is right.
3. **The current slot's `object+0x4c` / `MEDIA_CUSTOM_CONTENT_INFO+0x28` pokes.**
   Decompile-derived offsets into LG-private C++ objects, used by the in-place seek. No symbol table
   can confirm them on another firmware, and a field added to `StarfishMediaAPIs` by any 5.x build
   moves `+0x4c`. Failure is a SIGSEGV, not a wrong answer. *Settled by:* seeking on a webOS 5 set.
4. **The keyboard event offsets.** `app.rs` reads state/wcode/sym at +16/+20/+24 because LG's SDL
   inserts `Uint32 inputSource` after `windowID`. That patch is applied by the openlgtv NDK to
   modern SDL2 as well, so the struct offsets very likely hold — but which value LG writes into
   `scancode` on webOS 5 is unverified, and the symbol database stores symbol names, never layouts.
   *Settled by:* pressing a key and reading the raw bytes the log already prints.
5. **Is the opaque-region clear still needed?** LSM composites fullscreen app surfaces with
   `useTextureAlpha = true` regardless, and Kodi sets a *full* opaque region on webOS and still
   punches through. Probably unnecessary on 5+; kept because it is cheap and cannot hurt. The real
   requirement is the 32-bit RGBA config plus alpha=0 pixels in the video rect.
6. **webOS 10.2.0 and 11.2.0** (libavformat 59/60). The binary loads there, and `ff::boot` refuses
   to demux because there is no table. Reaching them needs a third one, and it is a bigger step
   than 58 was: `FF_API_CONVERGENCE_DURATION` and `FF_API_AVPICTURE` both die at 59, so
   `sizeof(AVPacket)` and the whole `AVSubtitleRect` layout move for the first time. `av_register_all`
   is also gone, so the loader reports `Incomplete` and names it.

### What the firmware symbol tables say about the 4-vs-6/10 difference

Asked of the databases rather than a disassembler, since we have symbol tables for all 14
firmwares and binaries for none. Three facts, none of which is the buffering bug, all of which
change what we know:

**`StarfishMediaAPIs` grew a windowId API at exactly 5.3.1.** `setWindowId(const char *)`,
`setSink(const char *)` and `setHdrInfo(const char *)` are absent on every release through 4.10.0
and present on every release from 5.3.1 — the same release `libAcbAPI` disappears. That is the
replacement surface, and it is worth knowing it exists.

It is NOT what we are missing: neither Kodi (`MediaPipelineWebOS.cpp`) nor ss4s
(`smp_player.c`, `smp_resource_webos5.c`) calls `setWindowId` at all — both pass the id only as
`option.windowId` in the Load payload, and both ship on modern webOS. Our approach matches the two
implementations known to work. `setWindowId` is a fallback to try if the payload route turns out
not to be enough, not a correction.

**`mediapipeline::CustomPipeline` gained 78 exported methods on 6.4.0 and 92 on 10.2.0** —
`createPipeline`, `requestResource`, `setAppSrcCaps`, `removeAudioBin` and the rest. A class that
grew that much has almost certainly changed layout, which is the strongest evidence yet that
`starfish.c`'s current-slot `object+0x4c` → `player+0x04` walk to reach it must not run on those
firmwares. The in-place seek that depends on it is now disabled on `VP_EXPORTED` for exactly that
reason.

**The whole of libplayerAPIs+libpf moved a long way**: 2144 exported symbols on 4.10.0, 2736 on
6.4.0, 2628 on 10.2.0. `FeedStream` even gains a second overload with four more arguments on
10.2.0. We call none of those directly — `Feed` takes JSON — but it is the scale of the change
that matters when judging how much a webOS 4 assumption is worth elsewhere.

## 5. Things a webOS 5 port must NOT forget

- **The event-type numbering shifts by 2** between webOS 4 and 5 for every `StarfishMediaAPIs`
  callback above `PF_EVENT_TYPE_STR_STATE_UPDATE__ENDOFSTREAM` (0x1c). Kodi compensates with
  `if (m_webOSVersion < 5 && type > …ENDOFSTREAM) type += 2;`. **This app is currently immune** —
  `sf_on_event` dispatches on string content, and its one numeric test (`ty == 0`, FRAMEREADY) is
  below the shift point. Any *new* numeric event handling must account for it.
- **`StarfishMediaAPIs::setHdrInfo(const char*)` exists from 5.3.1 on** and is absent on 4.10.0
  (which confirms this repo's earlier finding that it could not be used). On webOS 5 it replaces
  the webOS-4 trick of injecting `hdrType` into the ACB `setMediaVideoData` payload. The JSON key
  names differ by era: `mediaSei`/`mediaVui` below 5, `sei`/`vui` from 5.
- **The exported window has a SUBTITLE type** (value 1) that webOS 4's ACB had no equivalent for.
  Whether it opens a route to the TV's hardware subtitle engine in buffer-feed mode — currently
  documented here as URI-mode only — is unexplored.
- **Codec capability is still asserted, not probed.** `profile_extra()` tells PMS "HEVC + 4K +
  10-bit" unconditionally because that is what the author's panel does. On a lower-end webOS 5 set
  that is wrong, and so is the transcode fallback, which also targets HEVC.
- **Resolution: do NOT use `SDL_webOSGetPanelResolution`.** It exists on both eras, which makes it
  an inviting answer and the wrong one — it reports the **panel**, not your drawable. Measured on
  the dev TV (a 4K set): `window=1920x1080 drawable=1920x1080 panel=3840x2160`. Sizing the UI from
  the panel would render a 4K interface into a 1080p buffer.

  The 1920×1080 authoring canvas is correct and stays. `surface.rs` now reads the real drawable and
  `glViewport` follows it, with `u_screen` staying logical so the shaders' own divide does the
  mapping — an unexpected surface therefore scales the interface rather than parking it in the
  bottom-left corner. `scale()` is 1.0 on every device seen so far.

  **Rendering at panel resolution would be the wrong trade even where offered.** 60 fps at 1080p on
  this Mali part took real optimisation work; 3840×2160 is four times the pixels through the same
  shaders, while the compositor's upscale is free and in hardware.

## 6. Emulators — no, and the reason is not a gap in the search

Re-checked 2026-08-05; unchanged from the 2026-07-28 survey in `docs/distribution.md` §3.4a.

- **LG TV Simulator** — "Native apps are not supported (only web apps are supported)", verbatim.
  Latest v1.5.0 simulates webOS 6.0 and up, so it would not cover 5.x even if it could.
- **LG TV Emulator** — deprecated ("From webOS TV 22, Emulator will not be provided"), x86-64
  guest under VirtualBox, macOS support is "Intel chip (Apple silicon is not supported)". A
  webOS 5.0.0 image *is* listed — a small correction to §3.4a — but it is x86, so an armv7 ELF
  cannot execute on it regardless.
- **webOS OSE** (2.28.0) — ships no `libAcbAPI` and no `libplayerAPIs`; those are LG's proprietary
  TV media stack and they are the entire playback path here. Targets are `qemux86-64` and
  `raspberrypi4-64`; there is no armv7 target at all.
- **qemu-user** — the only option with any value, and only a narrow one: it could run the host test
  suite against the real 32-bit ARM Linux ABI, and exercise `dlopen` SONAME fallback against fake
  `.so` files. It needs a Linux VM first (QEMU enables `linux-user` only when the *host* is Linux).
  Not built.
- **Nothing off-device can execute LG's media stack.** On 5.3.1, `libplayerAPIs.so.1` needs 22
  libraries — the whole luna / uMediaServer / SoC chain.

The honest framing: **an emulator was never the thing that would have helped.** The firmware symbol
databases answer the load-time questions better than an emulator would, and no emulator answers
"does a picture appear" either. What is needed is one television.

## 7. If you have a webOS 5+ LG and want to help

This is the single most useful contribution available to this project.

1. Install the `.ipk` (Homebrew Channel, or dev-manager-desktop). It will start — that much is
   proven statically.
2. Sign in, browse, press play on anything.
3. Send `/tmp/plxnative-events.log`. The first 30 lines settle most of §4 on their own:
   `vplane:` says which binding was chosen, `ff: bound …` says which FFmpeg SONAMEs resolved,
   `ff: ABI table …` says which offset table is in force, and `wm sdl=…` says whether the
   transparency handshake worked.

Even "it starts and the UI works but the video is black" is a decisive result — it separates
§4.1 from everything else in one report.
