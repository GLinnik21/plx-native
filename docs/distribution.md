# Distribution: what it takes to ship this publicly

Researched 2026-08-01 against live primary sources (webosbrew apps-repo + schemas, LG developer
docs, Plex ToS/trademark pages, LGPL-2.1 text, FFmpeg legal.html, TMDB/Fandango terms) and against
this repo's own files. Every non-obvious claim below carries either a URL or a `file:line`.

Three claims were independently re-researched by an adversarial pass whose job was to refute them;
where that pass changed the answer it is marked **[corrected]**.

---

## Bottom line

| Question | Answer |
| --- | --- |
| Ship via **webOS Homebrew Channel**? | **Yes** — it is a real, current, native-app-friendly path. ~1 day of packaging work plus the licence fixes below. |
| Ship via **LG Content Store**? | **No.** Not for a hand-written native binary, not under any partner tier, not at any price. Would require a rewrite as a web app. |
| Runs on **newer webOS**? | 32-bit armv7 is *not* the problem — it stays the native userland through webOS 26. **ACB is the problem**: `libAcbAPI` exists on webOS 4.x only. Today's binary does not reach `main()` on 5.0+. |
| Is **private data** in the repo? | Git history is clean of credentials. **Mostly resolved 2026-08-02:** the 252 (not ~40) `/Users/gleblinnik/…` paths are remapped to 0, and both runtime surfaces — the squattable `/tmp` FIFO and the unauthenticated `0.0.0.0:8910` listener — are compiled out of a release build along with the whole trigger surface. Left: the **event log is still written by every build** and names the server, its LAN address, profile names and episode titles into a world-readable `/tmp` (§4 — the one item on that list not compiled out); `192.168.0.3` is still baked into a binary built on *this* machine (release comes from CI, which has no `config.local.h`); and the token still wants rotating. |
| Needs a **rooted** TV? | **No** — device-proven, see §3.5. But the app currently only works under the **Dev Mode** install prefix; a Homebrew-Channel install breaks fonts and login. |
| **Licensing** clear? | Font blocker **CLEARED 2026-08-01** — Inter (OFL 1.1) replaced Monotype Arial. FFmpeg/LGPL is fine. The repo still has **no LICENSE file at all**. |
| **Trademarks**? | Plex itself is fine (their guidelines have an explicit permitted formula). **Rotten Tomatoes marks have no licensing route that exists**, and the TMDB logo ships without its mandatory attribution. |

**Blockers, in the order they'd bite:** *(1 — the Arial font — was cleared 2026-08-01, see §5.3)*

2. No `LICENSE` file → all-rights-reserved → ineligible for webosbrew `pool: main`, and no licence for anyone who clones the repo.
3. ~~10 icons reproduce Rotten Tomatoes / TMDB / IMDb marks, tinted in those brands' exact hex.~~
   **CLEARED 2026-08-02** — see §11.
4. `X-Plex-Product: Plex for webOS` — Plex's own first-party naming pattern, on a platform that has an official Plex app.
5. Release builds bake in the dev's LAN IP and home-directory paths.
6. `remote.rs:36-38` creates/opens `/tmp/plxnative-remote` unconditionally every boot (`app.rs:1603`) without checking ownership — squattable on a shared `/tmp`.
7. **The app only works under the Dev Mode install prefix.** A Homebrew-Channel install lands at
   `/media/cryptofs/…`, where the hardcoded font paths silently fall back to DroidSans and the
   session file cannot be written at all — a permanent re-login loop. See §3.5.

---

## 1. webOS Homebrew (webosbrew) — the actual requirements

Live and healthy: `github.com/webosbrew/apps-repo` had commits on 2026-07-31; 45 packages in `main`.
Native is first-class — Moonlight (SDL2/C), Kodi, RetroArch and IHSplay all ship as
`type: "native"` ARM ipks. **There is no Plex client of any kind in the repository today.**

### 1.1 The submission is one file

Fork `webosbrew/apps-repo`, add `packages/com.beb.plxnative.yml`, open a PR. The filename stem must
equal the manifest `id` (`repogen/lintpkg.py` hard-errors otherwise). Schema is
`content/schemas/packages/PackageInfo.json`, draft 2020-12, **`additionalProperties: false`**:

```yaml
title: PlxNative                       # required, <=30 chars
shortDescription: Native client for Plex Media Server   # optional, <=80 chars
iconUri: https://…/icon160.png         # required; https: or data:, must return 200
detailIconUri: https://…/icon320.png   # optional, 320px convention
manifestUrl: https://github.com/<you>/<repo>/releases/latest/download/com.beb.plxnative.manifest.json
category: multimedia                   # required
pool: main                             # required; main = MUST be open source
requirements:
  webosRelease: '>=4.0, <5.0'          # see 1.4 — omit this and CI fails
description: |                         # required; Markdown, every <img> must be https
  …
funding:
  github: [ GLinnik21 ]                # optional
```

There is deliberately **no place in the YAML for a version, ipk URL or hash** — those live only in
the manifest you host yourself.

### 1.2 The manifest you host

`content/schemas/api/PackageManifest.schema`. Required: `id`, `version`, `type`, `title`, `iconUri`,
`ipkUrl`, `ipkHash`. Optional: `rootRequired`, `sourceUrl`, `appDescription`, `ipkSize`,
`installedSize`. `type` enum includes `native`. Generate it rather than hand-writing it:

```
webosbrew-gen-manifest -p pkg/com.beb.plxnative_0.1.0_arm.ipk \
  -i https://…/icon160.png -l https://github.com/<you>/<repo> -r false \
  -o com.beb.plxnative.manifest.json
```

(from `webosbrew/dev-toolbox-cli`). It reads id/version/type/title out of the ipk's embedded
`appinfo.json`, computes the sha256, and sets `ipkUrl` to the **bare filename** — which resolves
relative to the manifest URL on both server and client. So attach **both** the `.ipk` and the
`.manifest.json` to the same GitHub Release and `releases/latest/download/…` becomes self-updating.

**The sha256 is enforced on the device, not in CI.** `services/service.ts` downloads, hashes, and
aborts on mismatch. CI never checks it. Consequence: overwriting a release asset without
regenerating the manifest keeps CI green and breaks every user's install. There is no code signing
anywhere in this chain — sha256 over HTTPS is the entire integrity story.

### 1.3 CI is real and gating

`pr-check.yml` → `package-lint.yml`, three stages, non-zero exit blocks the PR:

1. `repogen.lintpkg` — schema, `pool` value, manifest-id == package-id, `iconUri` returns 200, every
   `<img>` in the description is https.
2. `repogen.downloadipk` — must fetch.
3. `repogen.check_compat` → **`webosbrew-ipk-verify`** — statically resolves your ELF's `DT_NEEDED`
   and symbols against **14 real firmware library databases** (starfish 1.2.0 … 11.2.0).
   `is_good() = missing_lib.is_empty() && undefined_sym.is_empty()`. A missing library or an
   eagerly-bound undefined symbol → Failed → CI red.

### 1.4 This is where this project gets caught

Our actual `DT_NEEDED` (verified with the NDK readelf on `pkg/plxnative`):

```
libSDL2-2.0.so.0  libSDL2_ttf-2.0.so.0  libGLESv2.so.2  libluna-service2.so.3  libglib-2.0.so.0
libAcbAPI.so.1    libwayland-client.so.0  libplayerAPIs.so.1  libpf-1.0.so.1
libavformat.so.57 libavcodec.so.57  libavutil.so.55  libswscale.so.4  libcurl.so.5
libdl/libpthread/libm/libgcc_s/libc/librt/ld-linux.so.3
```

Against the compat DBs: **all present on releases 4.4.2 and 4.10.0 only.** On 5.3.1/6.4.0 the four
FFmpeg libs and `libAcbAPI.so.1` are missing; from 7.4.0 up `libcurl.so.5` goes too. So a bare
submission is checked against all 14 firmwares and fails.

**Mitigation: `requirements.webosRelease: '>=4.0, <5.0'`** — `repogen/check_compat.py` wires that
string straight to `ipk-verify --fw-releases`, scoping the check. The repo already ships packages
using this (Kodi `'>=4.0'` + a `deviceSoC: ['!k5lp']` exclusion).

Note the stub-`.so` trick does **not** help here: the stubs are link-time only and are never in the
ipk, so the verifier sees a genuine unresolved `DT_NEEDED`.

Also note `requirements` does **not** filter the Homebrew Channel's app list — I grepped the whole
HBC frontend and nothing consumes it. **webOS 6+ users will install this app anyway**, so it needs a
graceful on-device failure, not a `SIGSEGV`.

**Run the gate locally before opening the PR** — this is the single cheapest de-risking step and it
has never been run:

```
webosbrew-ipk-verify --details --fw-releases '>=4.0, <5.0' pkg/com.beb.plxnative_0.1.0_arm.ipk
```

### 1.5 The four repository rules — one of which is new and directly relevant

`README.md`, verbatim:

> 1. **NO AI SLOP.** Applications made primarily by AI and submitted without meaningful human
>    development, testing, and review will be rejected. AI tools may be used, but the submitter is
>    responsible for the code and must disclose that use in the pull request.
> 2. **NO PIRACY.** …
> 3. Be considerate to users' TV. It's not a cheap toy, so try your best not breaking it.
> 4. If you are making a port to existing applications, please make sure that you are following the
>    original project's open source license.

Rule 1 landed 2026-07-22 (PR #207) — the README had not been touched since 2023. The PR template
carries a six-way AI-use declaration; the honest and **explicitly permitted** box here is *"AI coding
agents were used; an experienced developer has reviewed and tested all generated code."* The box that
gets you rejected is the one below it. The 18-case on-device suite and the device-verified ABI work
are the evidence that supports the permitted box. The template also requires ticking *"I have tested
this application on a real webOS TV."*

Rule 2 is aimed at IPTV/DRM circumvention. A self-hosted-media-server client is an accepted category
— Jellyfin, Moonfin, Litefin, Breezyfin, Subsonic and two Immich clients are all in `main`.

### 1.6 `appinfo.json` and packaging

Verified against Moonlight's shipped native app (the reference implementation for SDL2-on-webOS):
`icon` = **80×80** PNG, `largeIcon` = **130×130** PNG. Ours already are (checked the IHDRs).
`version` must be exactly three integers. `main` is the executable path.
`nativeLifeCycleInterfaceVersion` is a **webOS OSE** property — no shipped native TV app sets it;
ours does, harmlessly.

`rootRequired: false` is right *in principle* — but see the open question in §6.

The registry reads metadata out of the ipk (`repogen/ipk_file.py` opens the `ar`, reads
`Package`/`Version`/`Installed-Size`, then `appinfo.json`), so the control file, `appinfo.json` and
the manifest must agree. Our `ar` format is already correct (GNU, via the NDK's `ar`).

### 1.7 End users

Rooting is **not** required to install: LG Developer Mode works, and both Kodi and RetroArch list
"Root: Not required". The cost is that Dev Mode is session-limited (LG's page: apps "are
uninstalled" when the session expires; webosbrew documents 1000 hours, Moonlight's docs say 999 and
also "10 reboots without network connection"). Rooting is largely patched on current firmware
(faultmanager-autoroot, Aug 2025: webOS 5/6/7/9 latest firmware patched; on 2025 models only
factory webOS 10.0 is vulnerable) — but **webOS 4.5, our own target, is unpatched**.

---

## 2. LG Content Store — no

**[corrected]** The adversarial pass found one part of the naive answer is now stale, so state it
precisely:

**What's true:** LG's own `appinfo.json` reference still says of `type`: *"Only `web` is allowed
currently"* (webostv.developer.lge.com/develop/references/appinfo-json). LG staff on their own forum,
repeatedly: *"webOS TV apps are web-based… if you can run your app in a web browser you can run it on
webOS TV"* (2024-01-30). A developer who asked Seller Lounge to sign a partnership for NDK access was
told *"the NDK feature cannot be supported even when the deal proceeds"* (2023-07-12 — a forum user
quoting an email, so: reported practice, not documented policy). webosbrew states flatly *"Native
application development is not officially supported."*

**What changed [corrected]:** LG **does** now publish a webOS TV NDK publicly on GitHub with no NDA
or login — `github.com/lg-flutter-webos/ndk`, release 11.2.0 published 2026-06-29, a 340 MB starfish
`ca9v1` **32-bit ARM** cross toolchain, the same class of toolchain we already use. LG documents
"Flutter for webOS" as an official Develop guide (announced 2026-06-30), and **LG's own project
template writes `"type": "native"` into `appinfo.json`**. So LG's docs now contradict each other.

**Why it still doesn't help us:**
- Flutter for webOS requires **webOS 26 Re:New**. Nothing for a 2019 TV.
- LG scopes the NDK to *"building Flutter applications on webOS."* No LG source documents submitting
  a hand-rolled Rust/SDL2 binary.
- Whether even a Flutter ipk can be *submitted to the store* is **UNVERIFIED** — no LG page (Flutter
  guide, the news post, getting-started, App Approval Process, App Ecosystem, public Seller Lounge)
  mentions store submission of non-web apps in either direction.

The submission pipeline itself is otherwise open: Seller Lounge account (individual or corporate),
ipk ≤ 2 GB, mandatory UX-scenario doc + self-checklist, three-stage QA. Hard UX rules include: every
selectable element must respond to 4-way + OK + Back, and on webOS 23–25 Back on the entry page must
show the Home screen.

**Content policy is not the obstacle** — Jellyfin and Emby both ship on the LG Content Store, so
"client for a self-hosted media server" is an accepted category. Plex ships an official webOS app
covering webOS 3/4/5. The obstacle is purely that this is a native binary.

**If store presence ever becomes the goal it means a rewrite as a web app**, which is what Plex,
Jellyfin and Emby all did. That is a different product, not a port.

---

## 3. Portability and newer webOS

### 3.1 The 32-bit question: settled, and the good way round

**[corrected]** The pessimistic reading ("stranded on 2018–2019 hardware") is wrong. LG ships a
**32-bit armv7 userland on every generation through webOS 26** — the libc package is literally
`lib32-glibc`, the loader is `ld-linux.so.3`, and webosbrew's NDK says it outright: *"LG's real
devices are using armv7a (with arm64 kernel)."* The kernel is arm64; the userland is 32-bit and is
the **only** userland. It is *aarch64* binaries that need a shim — hence
`webosbrew/webos-bridge-64to32` (created 2026-05-29, "tested on webOS 10 and 11" = webOS 25 and 26),
which exists precisely because the EGL/GLES libraries on the newest firmware are still 32-bit.
Kodi's armv7 ipk on repo.webosbrew.org lists `webOS >= 4.0` with **no upper bound**, updated
2026-07-30.

glibc goes 2.24 (4.x) → 2.28 → 2.30 → 2.35 → 2.39 (webOS 26); building against 2.12 is
forward-compatible by symbol versioning. SDL2 and SDL2_ttf ship on every firmware and get *newer*
(2.0.4 → 2.0.10 → 2.0.14). All 15 mangled StarfishMediaAPIs symbols we bind are present unchanged
from 4.10 through 11.2, and Kodi's current master still drives BUFFERSTREAM buffer-feed the same way
we do.


> **SUPERSEDED IN PART, 2026-08-05.** Sections 3.2 through 3.4 below were written when the port was
> hypothetical. It is no longer: the app now starts on every release from 4.4.2 to 11.2.0, the
> FFmpeg offsets are tabled per major and proven by the compiler, and ACB has a webOS 5 counterpart
> in the tree. **`docs/webos5-port.md` is the current record** — including three corrections to what
> is below (libcurl, 3.9.2, and the "highest-leverage fix" note, which has been done). What remains
> true here is the *shape* of the problem and the effort estimates for webOS 10.2.0+.

### 3.2 What actually breaks

| Seam | webOS 4.x | 5.0 / 6.x | 22–26 |
| --- | --- | --- | --- |
| armv7 userland | ✅ | ✅ | ✅ |
| SDL2 / SDL2_ttf | ✅ | ✅ | ✅ |
| `libplayerAPIs` + BUFFERSTREAM | ✅ | ✅ | ✅ |
| **`libAcbAPI.so.1`** | ✅ | ❌ **gone** | ❌ **gone** |
| **FFmpeg SONAMEs** | 57/57/55/4 | 58 | 59 → 60 |
| **`libcurl.so.5`** | ✅ | ✅ (compat alias) | ❌ → `.so.4` |

Three mechanical breaks, one of which is architectural:

1. **ACB disappears at exactly webOS 5.0**, replaced by the Wayland `wl_webos_foreign` exported-window
   protocol for video-surface export. This is our entire video-plane binding. Consistent with it:
   Kodi supports webOS 5+ and does **not** use ACB. *(Confidence: the per-firmware library index is
   hard evidence that `libAcbAPI.so.1` is absent; the `wl_webos_foreign` replacement is
   developer-forum evidence, not LG documentation.)*
2. **FFmpeg majors move**, which destroys hardcoded struct offsets — and ours are hardcoded to n3.3.
3. **libcurl flips SONAME** at webOS 22 — **corrected 2026-08-05: it flips at 7.4.0.** The file
   becomes `libcurl.so.4.5.0` at 5.3.1, but LG kept a `libcurl.so.5` compat *alias* beside it on
   5.3.1 and 6.4.0, so the name still resolves there and the real break is one release later.

Net: today's binary **does not reach `main()`** on anything past webOS 4.x — 5 missing libraries on
5/6, 6 on 22–26.

**Confirmed by tool 2026-08-02** (§9). `webosbrew-ipk-verify` grades the real binary against all 14
firmware databases: **OK on 4.4.2 and 4.10.0, FAIL on every other**, which is exactly the table
above and makes `webosRelease: '>=4.0, <5.0'` a measured bound rather than an inferred one. Two
refinements the table understated:

- **5.0 is not only an ACB break.** On 5.3.1 the missing set is `libAcbAPI.so.1` **plus all four
  FFmpeg SONAMEs** (57/57/55/4) — the major bump and the ACB removal land in the *same* release, so
  "port ACB to `wl_webos_foreign`" is not on its own enough to reach webOS 5.
- **3.9.2 fails too**, so the supported window has a bottom as well as a top and the `>=4.0` half
  of the requirement is load-bearing. ~~(`libAcbAPI.so.1` absent)~~ — **that reason was WRONG,
  corrected 2026-08-05**: 3.9.2 *has* `libAcbAPI.so.1`. It fails on the FFmpeg SONAMEs (it ships
  55/55/52/2, not 57/57/55/4) and on `StarfishMediaAPIs::Feed`, which carries a pre-C++11
  `std::string` mangling there. Check either with `tools/fwcompat.py --release 3.9.2`.

### 3.3 The pinning inside our own code (`audit:portability`, file-cited)

- **ABI, the worst layer.** ~15 raw FFmpeg struct byte-offsets with **no runtime gate** —
  `ff.rs` `OFF_STREAM_CODECPAR = 708`, `OFF_CTX_WIDTH = 124`, `OFF_CTX_HEIGHT = 128`,
  `OFF_CTX_PIX_FMT = 144`. `ff::boot()` (`ff.rs:934`) logs the library versions and **gates nothing**.
  On a different build these are *silent memory corruption*, not clean failure. Worse,
  `ff.rs:538` — the "ABI self-check" itself — stack-allocates an `AVCodecParameters` and passes it to
  `avcodec_parameters_from_context`; that struct grew in FFmpeg 4.x, so the self-check is the thing
  that overflows first. (`avcodec_parameters_alloc` already exists at `ff.rs:258` and is used
  elsewhere precisely because the true size is 136.)
- 72 stubbed FFmpeg/curl symbols link unconditionally; a missing symbol on a different firmware is a
  runtime abort, not a link error.
- The Starfish/ACB seam pokes decompile-derived offsets into private LG objects (`g_smp+0x4c`,
  `MEDIA_CUSTOM_CONTENT_INFO+0x28`).
- **Resolution is a compile-time constant everywhere.** The tree contains **zero** calls to
  `SDL_GetWindowSize`, `SDL_GL_GetDrawableSize` or `SDL_GetCurrentDisplayMode`.
  `glViewport(0,0,1920,1080)`, the scissor math, `u_screen` and `acb_start(0,0,1920,1080)` all assume
  the surface *is* 1080p. Whether webOS normalises a native app's graphics plane to 1080p on a 4K
  panel is **assumed, not verified** — and the severity of this finding hangs entirely on that.
- **Codec capability is asserted, never probed.** `profile_extra()` tells PMS "HEVC + 4K + 10-bit"
  unconditionally, and the transcode *fallback* target is also HEVC — so a lower-end panel has no
  working path at all.
- `sys_grab_wayland` hardcodes SDL version bytes 2/0/4 and never checks `subsystem`; a newer SDL
  silently returns a null surface and **video plays invisibly under an opaque UI plane**.

~~**Highest-leverage fix, ~10 lines:** make `ff::boot()` compare the runtime
`avformat_version()`/`avcodec_version()` against the offsets' provenance and **refuse to start**
instead of corrupting memory.~~ **DONE** — the refusal landed in `363548b`, and `4c3e237` went
further: the majors now *select* one of two offset tables, each asserted at build time against
real upstream headers. See `docs/webos5-port.md` §3.2.

### 3.4 Effort to reach newer webOS

- **Another webOS 4.5 model** — near-zero code work; untested (n=1 device), and the 4K-panel
  assumption is the thing to check first.
- **webOS 5/6** — replace ACB with `wl_webos_foreign`, re-derive FFmpeg offsets for the shipped
  major, gate the ABI. Days-to-weeks, needs the hardware.
- **webOS 22–26** — the above plus the libcurl SONAME flip. **Not** a rewrite, and **not** a 64-bit
  port. What actually bites on modern firmware is permission/API drift, not the instruction set: both
  real webOS-24 breakage reports found were a jail-permission regression (`/dev/dma_buf_unified`
  became inaccessible to libmali, EGL init failed) and a video-decode regression.

### 3.4a No emulator substitutes for the hardware (researched 2026-07-28)

Recorded here because the README tells webOS 5+ owners that their *hardware* is the blocker, and a
reader is entitled to ask why a virtual machine will not do. It will not, for four independent
reasons, any one of which is sufficient:

- **LG's TV emulator is x86 and deprecated.** It boots a webOS image under VirtualBox on an x86
  host. This app is an ARM ELF; there is no ARM emulator image, and none is published for 4.5.
- **The webOS TV Simulator runs web apps only.** It is a desktop application that hosts the web
  runtime. It has no mechanism to execute a native `.ipk` payload at all.
- **webOS OSE has neither Starfish nor ACB.** The open-source edition ships no
  `libplayerAPIs`/`libAcbAPI`: those are LG's proprietary TV media stack, and they are the entire
  playback path here. A build could link (the NDK sysroot has real 69 KB / 105 KB libraries to link
  against) and would then have nothing to bind to at runtime.
- **Apple Silicon cannot run AArch32 at all**, so the dev machine cannot even host a 32-bit ARM
  guest without full software emulation of a device tree that does not exist.

So the honest position is the one the README states: this needs a real webOS 5+ television, and
the author does not have one.

*Caveat on method:* this was a documentation and tooling survey, not an attempt to stand an
emulator up. If someone gets a native `.ipk` to execute under any LG emulator, that is a finding
worth having and this section is wrong.

### 3.5 Root is NOT required — but the install *prefix* is the live bug

**Device-proven 2026-08-01 on the 49SM9000PLA.** The running app is `Uid: 6910`, `Gid: 5000`,
`CapEff: 0`, chrooted to `/var/palm/jail/com.beb.plxnative`, with `libplayerAPIs`, `libAcbAPI`,
`libmali`, `libEGL/GLESv2`, `libwayland-webos-client`, `libavformat.so.57` and `libcurl.so.5` all
mapped from inside that chroot. Root on the dev TV buys the **ssh dev loop**, not app privilege.

Evidence is untainted, checked explicitly: `md5sum /media/developer/jail_app.conf` =
`410cb71a593449bcaccf000134592c87` — **LG's stock webOS 4.5 profile**, not the
`032d779de16b537f4517a895d54058e3` that the crashd root exploit's `jailpatch.sh` installs.
`/var/luna/preferences/jailer_disabled` absent; `NoJailApps` in `sam-conf.json` contains only LG
system apps. So SAM really is jailing us.

**LG publishes the jail config itself** at
`https://developer.lge.com/common/file/DownloadFile.dev?sdkVersion=<ver>&fileType=conf`. The
relevant lines are unchanged from webOS 3.0 through 10.0:

```
mount rw /media/developer          # <- our auth.json location is LG's own config, not a root artifact
mount ro /media/internal
mount rw /tmp   +   chmod 1777 /tmp
groups video,audio,luna,compositor,crashd,se
```

The media stack is permitted by an **LS2 role file the Dev Mode installer writes automatically** —
nothing we declare. On the device, `/var/palm/ls2-dev/roles/pub/com.beb.plxnative.json` grants
`com.webos.media.client.*`, `com.webos.rm.client.*`, `com.webos.pipeline.*` with in/outbound to
`com.webos.media`. That is exactly the surface StarfishMediaAPIs needs, and it is why **no
`requiredPermissions` field is needed in `appinfo.json`** (neither Kodi nor Moonlight sets one).

Corroboration: Kodi's `MediaPipelineWebOS.cpp` drives `mediaTransportType: "BUFFERSTREAM"` and
`AcbAPI_initialize(…, PLAYER_TYPE_MSE, getenv("APPID"), …)`, and its own docs say *"you do not need
to 'root' your TV to install Kodi."* Moonlight's backend (`mariotaku/ss4s`) runs the identical
Starfish feed + full ACB bind. Both list **Root: Not required** on repo.webosbrew.org.

**But there are two jail profiles, chosen by install prefix, and we only work under one:**

| | `jail_native_devmode.conf` (`/media/developer/apps/…`) | `jail_native.conf` (`/media/cryptofs/apps/…` — the Homebrew Channel) |
| --- | --- | --- |
| `/media/developer` | `mount rw`, and it is `HOME` | **never appears in the file** |
| `/media/internal` | `mount ro` | `mount rw` |
| `/tmp` | rw, 1777 | rw, 1777 |

So a **Homebrew-Channel install breaks two things**:

1. **Fonts** — `text.rs:15-16` hardcode the `/media/developer/…` prefix. The fonts *are* in the ipk
   and *are* installed correctly (the payload paths are relative), but the code can't find them, so
   `font_at` falls through to `/usr/share/fonts/DroidSans.ttf` — and `init_text` still logs
   `ok=1`. Measured against Arial: −4.67% mean advance (−45.8% on `J`), +4.2% line box, 2792→873
   codepoints, **no bold companion** so every bold rung becomes synthetic emboldening applied after
   grid-fitting. The whole `theme::size` ladder and the light-hinting contract silently invalidated,
   and `tools/font-hint-audit.py` reads host files so it structurally cannot see it.
   (`Makefile:143-146` records this exact silent-DroidSans outcome happening once before.)
2. **Login** — `session.rs:35` writes under `/media/developer`, which **does not exist inside the
   production jail**. `open` returns ENOENT and `save()` discards it with no `else` and no log
   (`session.rs:150-158`) → the user re-does the QR sign-in every single boot, with a fresh
   `X-Plex-Client-Identifier` minted each launch. There is **no persistent writable directory common
   to both layouts**, so the path has to become a probed search order, not a constant.

**FIXED 2026-08-01** — `rust-modules/src/paths.rs`. The app directory is resolved from
`read_link("/proc/self/exe")` (not `$HOME`: LG's conf sets it twice and which one wins differs
between the profiles), `text.rs` builds the font paths from it, and `capture.rs` now shares the
same resolver instead of open-coding it.

The session file became a **probed search order**, because there is no single writable persistent
directory common to both layouts: `/media/developer/<id>-auth.json` (Dev Mode, survives a
reinstall) → `/media/internal/.<id>-auth.json` (the production jail's only rw persistent location)
→ `<appdir>/auth.json` → the legacy path, read-only, for migration. `peek()` takes the first that
exists *and parses* so a half-written file at a preferred location cannot shadow a good one;
`clear()` removes all of them or the search would resurrect a stale session; and `save()` now
**logs** when every candidate fails, turning an unexplainable infinite login loop into a
reportable bug. The DroidSans fallback logs once per boot for the same reason.

Verified on device: `appdir: /media/developer/apps/… (from /proc/self/exe)`, no `FONT FALLBACK`
line, real fonts rendering, and the session file rewritten `0600` under the app's own uid.

### 3.6 What the unrooted path costs the user

- **1000 hours ≈ 42 days.** At expiry LG's docs say *"the installed apps that you were using on
  Developer Mode are uninstalled, and you will be taken to the log-in screen"* — and *"if the session
  time runs out, you cannot extend"*. Dev Mode is also killed by ten reboots with no network, or by
  signing the same LG account into another TV (one developer account per TV).
- **It is renewable indefinitely, but only via undocumented automation:** a GET to
  `https://developer.lge.com/secure/ResetDevModeSession.dev?sessionToken=<token>` resets the
  server-side timer, and dev-manager-desktop ships a first-class "Automatic Developer Mode Renewal"
  dialog for it. Put that in the first-run flow, not in a README.
- **The realistic install tool is dev-manager-desktop**, not `ares-cli` (which needs npm plus an
  EOL Node 14–16). dev-manager-desktop is actively maintained, needs no LG SDK, and ships a
  universal macOS dmg / Windows MSI / Linux packages.
- **`make deploy` will not work for an unrooted user or contributor.** It scps into the app dir as
  root over port 22; a Dev Mode TV only offers `prisoner` on port 9922, and appinstalld leaves the
  app dir `root:root 775`. An unrooted iteration loop must go through a full ipk install every time.

---

## 4. Private data

**Git history is clean.** All 2,694 unique blobs across 36 local + 25 remote refs were swept
(`git cat-file` sweep + byte-exact needle search for the real values currently in the gitignored
`src/config.local.h`, + an entropy sweep). The real `PMS_TOKEN` and `DEMO_STREAM_URL` have **never**
been committed; `src/config.local.h`, `local.env` and any `auth.json` have **never** been tracked.
The only historical binaries are the two icons and one short-lived `pkg/threadprobe`. The
`accessToken: transient-…` and `10.0.0.x` strings in `docs/plex-openapi.json` are from the upstream
published spec, not this user.

*Caveat on method:* a needle search finds the **current** token. A previously-rotated token would not
be found this way. Rotate the token regardless — it has been on disk in many places and injected into
world-readable `/tmp` on the TV across many runs.

**Judgment calls, made explicitly:**

- `sshpass -p alpine` (`Makefile:24-25`) — **not a secret.** `alpine` is the published webosbrew
  dev-mode root password; the repo's own skill says so. Publishable. It does teach an insecure
  default and will authenticate against *any* rooted webOS TV a contributor points `TV=` at.
- `192.168.0.114` / `192.168.0.3` — RFC1918, low risk, but they are the maintainer's home topology,
  they are the defaults every contributor inherits, **and one of them gets baked into the binary**.
  **FIXED 2026-08-02.** `git ls-files | xargs grep '192\.168\.0\.'` is now empty outside this
  section, which keeps its citations on purpose. The TV's address moved to the gitignored
  **`.tv-host`**, which `Makefile`'s `TV` and `tools/`' `TV_HOST` both fall back to — so the loop
  is unchanged for anyone who has one, and a target that needs a TV without one fails saying so
  rather than dialling `root@`. Documentation and fixtures use RFC 5737 TEST-NET (`192.0.2.x`).
  `alpine` deliberately stayed (see above): it identifies nobody, and removing it protects nothing.
  The PMS address is the one that still reaches a *binary*, and only one built on this machine —
  see the leak list below.
- `glinnik21@gmail.com` (`ipkroot/ctl/control:7` — this said `:6`) — deliberate maintainer contact,
  but a personal Gmail shipping inside every distributed `.ipk`. **FIXED 2026-08-02:** replaced with
  the GitHub noreply alias, and `ci/check-package.py` now asserts the field is not a personal
  mailbox, so it cannot come back by an edit nobody reviews.

**What a public build would leak (verified by `strings` on `pkg/plxnative` and inside the ipk):**

1. `192.168.0.3` — `PMS_HOST` from `config.local.h` is compiled in. *(Only used on the
   `/tmp/plxnative-token` automation branch — `app.rs:436-438` — so a public build with no
   `config.local.h` compiles the `"YOUR_PMS_HOST"` placeholder and never uses it. Still, don't ship a
   binary built on this machine.)*
   **Measured 2026-08-02, and better than this list implied: the TOKEN never reaches the binary.**
   `PMS_TOKEN` is referenced nowhere outside `config.local.h` itself — `main.c` passes only
   `PMS_HOST`/`PMS_PORT` to `plex_run` — and `strings` finds zero occurrences of the real value.
   The leak here is one RFC1918 address, not a credential. Note also that the local guard is
   self-disabling: `ci/check-elf.sh` skips its config-dependent assertions when `config.local.h`
   is present, so `make ipk` on this machine still produces a fully valid, correctly-hashed
   package with that address inside it and every check passing. Release comes from CI.
2. ~~~40~~ **252** `/Users/gleblinnik/…` panic paths — **FIXED 2026-08-02**, count corrected by
   measurement (`strings -a pkg/plxnative | grep -c /Users/`): 113 from `-Z build-std` compiling
   std out of `$RUSTUP_HOME`, 139 from dependency panic locations under `$CARGO_HOME`. 250 of the
   252 are in `.rodata`, which is why **`strip` does not touch them** — this section used to list
   the two as adjacent chores and they are independent fixes. The Makefile now always sets
   `RUSTFLAGS` with three `--remap-path-prefix` entries; `$HOME` first, the specific roots after,
   because rustc applies the **last** match (measured — with `$HOME` last, `$CARGO_HOME` came out
   as `/build/.cargo` rather than `/cargo`). Now 0.

   Worth separating from privacy: this is also what made the Makefile's "same commit + same
   toolchain → same sha256" claim TRUE. While `$HOME` sat in `.rodata`, two developers at one
   commit produced different packages — and that hash is the entire integrity story for a user's
   install, since nothing in the webosbrew chain is signed.
3. The maintainer's Gmail, in the control file.

**Runtime surfaces that must not ship enabled:**

- `remote.rs:36-38` mkfifos `/tmp/plxnative-remote` and drains it every frame on **every boot with
  no trigger gate** (`app.rs:1603`). *Corrected 2026-08-01:* the requested `0o666` is masked by the
  process umask (0022), so the live FIFO on the device is `prw-r--r-- 6910:5000` — another uid can
  **read** it (stealing whatever the host driver writes) but cannot drive the UI through it. The
  real hole is **pre-creation squatting**: `/tmp` is mode 1777 in *both* jail profiles, `mkfifo`'s
  return is ignored by design (EEXIST is treated as fine), and the app then `open`s whatever object
  is already at that path. Any process that creates its own 0666 FIFO there first owns the UI.
  Fix: gate it behind a dev feature, and `stat` the path and refuse anything not owned by our euid.
- `capture.rs:208` binds `INADDR_ANY` with no authentication.
- **`/tmp` is the shared system `/tmp`, in the production jail too** — so the whole `/tmp/plxnative-*`
  trigger surface is squattable by any co-resident process on an ordinary unrooted TV.
  `/tmp/plxnative-token` beats the stored session outright (`app.rs:362`), and any unrecognised
  `plxnative-*` file suppresses the who's-watching picker (`app.rs:402-413`). The clean fix is a
  cargo feature the release build does not enable, so a public binary reads nothing from `/tmp`.
- **A crash writes a ~200 MB core into the app directory.** `main.c:75-77` restores `SIG_DFL` and
  re-raises (correct for crashd triage), the jail sets `setrlimit CORE INF INF`, and
  `/proc/sys/kernel/core_pattern` is the bare string `core` — i.e. relative to cwd = the app dir.
  Measured on the device: a **209,965,056-byte** `core` from Jul 18 is sitting there now, on a
  partition (`/dev/mmcblk0p53`, which backs both `/media/developer` and `/media/cryptofs`) with
  **615.6 MB total and 207.3 MB free — shared with every app installed on the TV.** One more crash
  fills it. This is squarely webosbrew rule 3 ("be considerate to users' TV").
  Fix: `setrlimit(RLIMIT_CORE, 0)` in a release build; the tracer's own PC+maps log is what triage
  actually uses.
- **STILL OPEN — the one unfixed item on this list.** The event log prints the server name + LAN
  address, Plex Home profile names and episode titles, and it is written by EVERY build: the four
  log sinks are deliberately outside the `devtriggers` gate (`dev.rs`'s module doc says why — they
  are how on-device crash triage works at all). `/tmp` is mode 1777 in the production jail too, so
  on a shipped install that file is world-readable. There is a token redactor on the *host* side
  (`tests/run.py` `RE_TOKEN`) and none on the device.

  Impact is bounded — it is read-only and needs code execution on the TV already — but its three
  neighbours above are now compiled out and this is not, so do not read the group as closed.
  The fix is a mode-0600 open plus dropping titles and profile names from the log, or logging
  ratingKeys instead of titles.

**Verified clean, no action:** no analytics, no telemetry, no crash upload; the only outbound hosts
are the user's PMS, plex.tv and `discover.provider.plex.tv`; TLS verification is on
(`net.rs:96-97`); `auth.json` is created `0600` via `OpenOptionsExt::mode` — correctly, in `open(2)`'s
own mode argument rather than create-then-chmod (`session.rs:154`).

**Personal-library disclosure:** `tests/manifest.json` is a 21-title inventory of a private media
collection with real `ratingKey`s, plus a real Plex Home managed-user id. Much of `docs/` quotes the
same. Sanitize or synthesize before publishing.

**Local hygiene (none of this is committed, all of it is on disk):** `main.o` at the repo root
contains the **real Plex token** in `.rodata`; `.claude/worktrees/` holds 17 copies of
`config.local.h` with the real token; `.tv-dpad.log` holds a live Basic-auth password. Also: 15
local-only + 4 pushed `worktree-agent-*` branches — never `git push --all`/`--mirror`, and check
`git remote -v` before flipping repo visibility.

---

## 5. Licensing

### 5.1 The project has no licence

No `LICENSE`, `COPYING`, `NOTICE` or `THIRD-PARTY-NOTICES` anywhere. `git ls-files | grep -i
'licen|copying|notice'` returns exactly one hit — `include/SDL2/SDL_copying.h`, which is SDL's
licence, not ours. `rust-modules/Cargo.toml` declares no `license` field. Default copyright applies:
**all rights reserved**, so nobody who clones it may use it — and webosbrew `pool: main` requires
open source.

**Nothing in the tree forces copyleft.** Recommend **MIT** (ecosystem norm for webosbrew apps,
lighter than Apache-2.0). The one constraint LGPL §6 places on the choice is that the terms must
"permit modification of the work for the customer's own use and reverse engineering for debugging
such modifications" — MIT and Apache-2.0 both clear it trivially; a proprietary EULA would not.

### 5.2 FFmpeg / LGPL — resolves in our favour, but not for the reason you'd think

**The TV's FFmpeg is LGPL-2.1+, verified from the device binary.** The build-config string in
`.abi-cache/libavcodec.so.57.89.100` (LG's own Starfish build, `lib32-ffmpeg/3.3-r0`) contains
`--disable-gpl`, no `--enable-nonfree`, no `--enable-version3`, and the binary embeds
`libavcodec license: LGPL version 2.1 or later`. **No GPL contamination risk.**

**[corrected]** The "we don't bundle it, so we owe nothing" framing is wrong on three counts:

- **The stub-`.so` mechanism is legally irrelevant.** The shipped binary carries real `DT_NEEDED`
  records for `libavformat.so.57` etc. It is, legally, an ordinary dynamically-linked FFmpeg client.
  LGPL 2.1 §8 makes *"link with"* a restricted act, so **§6 is the permission being relied on**
  whether or not we ship the library.
- **The right reason we owe no FFmpeg source is §6(b)** — a shared-library mechanism that uses a copy
  already on the user's system. The FSF states the outcome directly (gpl-faq #LGPLStaticVsDynamic).
  That conclusion stands.
- **"Only note the dependency" understates §6's chapeau**, which applies via route (b) too:
  (i) our terms must permit modification for the customer's own use and reverse engineering for
  debugging; (ii) **prominent notice with each copy** that the Library is used and is LGPL-covered;
  (iii) **a full copy of the LGPL-2.1 text must be supplied with the work**; (iv) if the app displays
  copyright notices at runtime, FFmpeg's must be among them with a pointer to the licence copy.

One live argument to be aware of: §6(b)(2) requires the mechanism "will operate properly with a
modified version of the library, if the user installs one, as long as the modified version is
interface-compatible." Our hardcoded struct offsets are an argument that it does not. No authority
tests this (UNVERIFIED — I found no case law or FSF commentary on private struct-offset access under
6(b)(2)), and §5's carve-out for "numerical parameters, data structure layouts and accessors" keeps
our *source* from being a derivative work. But it is one more reason to fix `ff::boot()` (§3.3).

FFmpeg's own checklist (ffmpeg.org/legal.html) says unconditionally *"Distribute the source code of
FFmpeg… Host the FFmpeg source code on the same webserver as the binary"* with no carve-out for
preinstalled libraries. That is copyright-holder expectation rather than licence text — and it is
cheap enough to just satisfy.

**Same treatment applies to glibc and GLib** (both LGPL-2.1+, both `DT_NEEDED`, both TV-provided).
`libgcc_s.so.1` is GPL-3 **with** the GCC Runtime Library Exception 3.1, and dynamically linked.

### 5.3 The font blocker — CLEARED 2026-08-01 (Inter)

**Landed:** `pkg/appfont{,-bold}.ttf` are now **Inter** (SIL OFL 1.1), static instances cut from the
variable font at **`wght` 400/700, `opsz` 18**, with `pkg/OFL.txt` shipped inside the ipk (the OFL
requires the licence to travel with the font). `ci/check-package.py`'s font check is no longer an
XFAIL — it is a real gate whose job is to stop Arial returning through a stale local copy, which is
exactly how it would come back, since the files are named `appfont*.ttf` either way.

Why Inter, and why `opsz` 18:

* Inter is the closest freely-licensed relative to **San Francisco** — x-height 52.2% / cap 72.8% of
  em, against SF Pro Text's ~52% / ~70%.
* At the default optical size Inter is **bar-heavy at BODY(28)**, and `theme.rs` states the contract
  plainly: *"rung values are chosen for hierarchy and legibility ONLY — no rung needs to dodge px
  sizes that hint badly."* Nudging was also ugly in both directions — BODY→27 sits 1 px from
  LABEL(26), BODY→30 sits 2 px from HEADLINE(32); both cost hierarchy.
* Sweeping the optical-size axis instead, **`opsz` 18 is the single clean point**: regular is
  bar-heavy only at LABEL(26), which was *already* an accepted carve-out under Arial, and bold is
  clean at every rung. Inter additionally **fixes MICRO(22)**, which Arial fails. So the audit
  passes with **no rung changed and the contract sentence still true**. (`opsz` 30 also cleans the
  regular face but breaks bold at 22.) Low `opsz` is Inter's *Text* end, which is the right choice
  for a screen read at 3 m regardless.
* **Coverage does not regress in practice.** Inter 2849 codepoints vs Arial 2830. The 1093 Arial
  codepoints Inter lacks are 500 Arabic + 133 Hebrew (already unrenderable — SDL2_ttf does no BiDi
  or shaping), 35 C1 control codes, and archaic Church-Slavonic/Coptic. Every character in
  `tests/manifest.json` renders.
* **Size:** 668 KB the pair, down from Arial's 1488 KB. Subsetting was measured (−83 KB) and
  rejected — not worth the `.notdef` risk on arbitrary PMS metadata for 2% of the package.
* **Reflow risk, measured:** Inter runs **+4.7% wider** than Arial on representative UI strings
  (+7.6% on `TV-MA`, −0.8% on `S2, E2`). Verified on device across detail / library: wrap points,
  ellipsis truncation, pill and button widths all unchanged, because the layout is content-sized
  rather than fixed-width. **Still outstanding: the player HUD and the account/settings popovers
  have not been swept.**

Still to do: purge the Arial blobs from git history (they are in every commit that touched `pkg/`),
which rewrites shared history and so needs an explicit go-ahead.

### 5.3.1 Historical — why Arial had to go

`pkg/appfont.ttf` and `pkg/appfont-bold.ttf` are **Arial**, not a look-alike. Two independent passes
parsed the `name` and `OS/2` tables:

```
[0]  copyright   © 2006 The Monotype Corporation. All Rights Reserved.
[1]  family      Arial
[3]  uniqueID    Monotype:Arial Regular:Version 5.01 (Microsoft)
[7]  trademark   Arial is a trademark of The Monotype Corporation…
[13] license     "You may use this font to display and print content as permitted by the license
                  terms for the product in which this font is included…"
achVendID = TMC,  fsType = 0x0008 (Editable Embedding)
```

They are git-tracked, `make deploy`-ed, and **inside `ipkroot/data.tar.gz`** — confirmed in the
built package. Microsoft's font-redistribution FAQ answers both relevant questions:
*"you may not redistribute the Windows fonts"*, and — on whether `fsType=8` helps — *"Can I embed the
fonts into a game, application or device I'm developing…? **No**, document font embedding permissions
relate to embedding fonts in documents only."*

**Fix:** swap to an OFL/Apache font, ship its licence next to the `.ttf` in the ipk, and re-verify
with `tools/font-hint-audit.py` (the rasterization contract in `theme.rs` depends on the metrics).

Three tracks proposed three different replacements; the constraint set that actually decides it:

- **Metric compatibility** matters — Liberation Sans / Arimo are metric-compatible with Arial, so the
  size ladder and the light-hinting audit carry over unchanged. Inter and Roboto are not, and will
  move every glyph run.
- **Cyrillic coverage is required** (implied by `text.rs`), which eliminates several candidates.
- Inter is OFL-1.1 with **no Reserved Font Name**, so renaming the file to `appfont.ttf` is fine.
  Roboto is Apache-2.0. SF Pro is a **second blocker** — Apple's licence forbids use on non-Apple
  operating systems.

Recommendation: **Arimo** (OFL, metric-compatible with Arial, full Cyrillic) as the low-risk swap;
Inter if you're willing to re-tune the size ladder.

### 5.4 Everything else in the tree

| Component | Licence | Obligation |
| --- | --- | --- |
| FFmpeg (avformat/avcodec/avutil/swscale) | LGPL-2.1+ *(verified on-device)* | Notice + full LGPL text in the ipk. No source. |
| glibc, GLib | LGPL-2.1+ | Same notice, same licence copy. |
| `libgcc_s` | GPL-3 + Runtime Library Exception 3.1 | None (exception covers it, dynamically linked). |
| SDL2, SDL2_ttf | Zlib *(SDL_ttf since **2.0.11**, not 2.20)* | None — clause 3 is a source-distribution clause. Listing is courtesy. |
| FreeType | FTL / GPLv2 | **Not triggered** — not in `DT_NEEDED`; it's transitive inside the TV's SDL2_ttf. Include the credit line anyway, it's one line. |
| nanosvg (vendored) | **Zlib**, Mikko Mononen | Keep the header intact (it is). **`src/svg.c:1` mislabels it "public domain" — wrong**, and a NOTICES file generated from that comment would omit Mononen and the AGG/Shemanarev credit. |
| libjpeg-turbo 2.1.4 | IJG + BSD-3 + Zlib | Currently **not** in the ipk (only `make deploy`-ed). The moment it ships: IJG attribution + the complete Modified BSD text. |
| jsmpeg (`tools/jsmpeg.min.js`) | MIT, Dominic Szablewski | **Live breach** — the minifier stripped the banner; 138 KB with no notice, and it's served over HTTP by two tools. Two-line fix. |
| Rust crates (30 third-party) | 100% permissive, **zero copyleft** | Standard MIT/Apache notices. `moxcms` and `pxfm` are `BSD-3-Clause OR Apache-2.0` — elect Apache to avoid BSD-3's binary-reproduction clause. |
| Rust std via `-Z build-std` | MIT OR Apache-2.0 | One notices entry. **Plus `compiler_builtins 0.1.160`, whose licence is a *conjunction*: `MIT AND Apache-2.0 WITH LLVM-exception AND (MIT OR Apache-2.0)`** — the LLVM-exception text must be reproduced. This is the most-missed entry in Rust NOTICES files. |
| `include/SDL2/*` | Zlib (stock upstream 2.0.4, **not** an LG fork) + Mesa MIT + Khronos MIT/Apache | Notices. |
| webosbrew NDK | n/a | A compiler doesn't licence its output. Just never add `-static` (that would pull LGPL §6(a) in for glibc). |

### 5.5 Trademarks — the part with real teeth

**Plex: fine, and there's an explicit permitted formula.** Their ToS (rev. 2025-10-30) defines
*"Interfacing Software"* to expressly include *"client applications that communicate directly or
indirectly with the Plex Solution."* A third-party client is a **named, anticipated category**, not
unauthorized access. Plex published official OpenAPI docs at developer.plex.tv in Sept 2025 naming
*"third-party clients that want to play content from a PMS"* as an intended audience; the PIN/JWT
device-auth flow we use is documented, with no API key or registration gate.

Four obligations attach that we don't currently meet:
1. A copyright notice **in the source code**, form `Copyright © <year> <holders>`.
2. A privacy notice (or link to one) summarizing practices consistent with Plex's policy.
3. Publishing grants Plex a worldwide royalty-free right to use, copy, display, market and
   **distribute** the app *and its name*, plus authority to sublicense it MIT-style to recipients.
   There is a 30-day opt-out. **This is a decision to make deliberately, not a footnote.**
4. "Your Interfacing Software" is an explicit indemnification trigger.

Naming: their guidelines list *"Use `for Plex` following the name of your application, provided that
the name of your application is unique"* under **You may**, and *"Use Plex or derivatives thereof in
the name of your application"* under **You may not**. `PlxNative` is a vowel-dropped "Plex" and sits
near the "derivatives / misspellings" line — grey, low risk, cheap to eliminate. Precedent is
reassuring: PlexKodiConnect (1.3k stars, "Plex" literally in the name) and Plezy (3k stars, four app
stores, listed as *"Plezy for Plex"*) both run unmolested. The one Plex DMCA found (2024-03-11,
`plex-reshare`) targeted a library-**resharing** tool, not a client.

**BLOCKING — `X-Plex-Product: Plex for webOS`** (`account.rs:42/46`, with
`X-Plex-Device-Name: Plex (LG webOS)`). That is exactly Plex's own first-party pattern (their
documented sample value is `"Plex for Roku"`), it surfaces in the account's authorized-devices list,
and LG **has** an official Plex app — so a server owner has no way to tell it isn't Plex's. Also
note `client.rs` uses a *different* product string (`PlxNative`): the plex.tv-facing and PMS-facing
identities disagree. *(Small caution: PMS transcode/direct-play decisions can key off client
identity, so re-run the player suite after renaming.)*

> **RESOLVED 2026-08-02 — this subsection is the ANALYSIS THAT LED TO THE FIX, not a description
> of what ships.** Everything below was true when written and is not true now: all ten Rotten
> Tomatoes icons and `tmdb.svg` were deleted, and the rating row names each provider in text with
> a verdict-only glyph beside the score. See **§11**. Kept because the reasoning — why redrawing a
> mark does not avoid the trademark, and why referential use in words does — is the useful part and
> is what any future brand asset should be measured against. Verify against `assets/icons/` before
> quoting anything here as current.

**BLOCKING — Rotten Tomatoes.** There is **no licensing route that exists**. The RT Developer Network
page states *"we no longer support unauthorized use of our data (e.g. unofficial projects)"*, the
Fandango Data Feed Terms licence the "RT Marks" only with a written authorization, and
`developer.fandango.com` **no longer resolves** (NXDOMAIN, verified) — so there isn't even an
application path. We ship 10 such icons (`tomato*.svg`, `popcorn*.svg`), and one carries the comment
*"Ported from Details Screen.dc.html, which draws RT's real Certified Fresh badge."*

**Redrawing them does not fix it.** Copyright covers Fandango's artwork files; **trademark** covers
the mark as a source identifier — and the entire function of the tomato, the green splat and the
Certified Fresh wreath is to signal "this is a Rotten Tomatoes rating." Fandango's terms bar
"derivative works based upon Fandango Property"; Plex's own guidelines make the same point
explicitly by prohibiting "any altered, distorted or interpreted representation." Rights holders in
this space target redraws.
**Fix (cheap):** render the score as text naming the source in words — "Rotten Tomatoes 91%" — which
is referential use, or use a neutral house glyph. The scores come from PMS either way, so dropping
the icons costs no data.

**BLOCKING but trivial — TMDB.** Terms (rev. 2023-10-20) §3 require the **unmodified** official logo
plus this notice placed prominently: *"This [application] uses TMDB and the TMDB APIs but is not
endorsed, certified, or otherwise approved by TMDB."* (Note this current wording differs from the
older short form that circulates.) Our `tmdb.svg` is a redraw — violating the no-modification rule —
and there is no attribution anywhere. Nuance: we don't call the TMDB API (we take
`themoviedb://image.rating` hints from PMS, `metadata.rs:322`), so we're likely not a licensee at all
and have no licence to the logo. Ship the official unmodified logo **with** the notice, or drop it.

Two more, minor: `assets/icons/user.svg` is character-for-character Google Material Icons `person`
(Apache-2.0, needs attribution) and `backspace.svg` is Feather Icons `delete` (MIT, needs the
notice). The other 29 are genuinely hand-authored. And `theme.rs` tints these marks in the brands'
**exact** hex — `#01B4E4` (TMDB), `#F5C518` (IMDb), RT red — which is what turns generic geometry
into a brand reproduction. `pkg/icon.png` is a solid `#E5A00D` tile — Plex's exact brand gold; not
protectable alone, but trade-dress-adjacent and a reviewer would notice.

---

## 6. Open questions — the things nobody has verified

Listed because each could change a decision above.

1. ~~Does this need root?~~ **RESOLVED 2026-08-01 — no.** See §3.5: device-proven jailed non-root
   execution against a verified-stock jail config, with LG's own published conf as the authority.
   `rootRequired: false` is honest. The question turned into a *different* and worse one, now
   tracked as blocker #7: the app only works under the **Dev Mode** install prefix, and the
   Homebrew Channel installs to the other one.
2. **Does webOS normalise the graphics plane to 1080p on a 4K panel?** The severity of the hardcoded
   `1920x1080` (cosmetic vs blocker) hangs entirely on this, and it's assumed.
3. ~~`webosbrew-ipk-verify` has never been run on the real binary.~~ **RESOLVED 2026-08-02 — it
   passes.** See §9; the run also found two packaging bugs that made the ipk uninstallable.
4. **LG's own terms are entirely uncovered** — nobody has read LG's SDK/NDK licence, the webOS TV
   EULA, or any anti-reverse-engineering clause in either. That is the open question, and it is
   genuinely unassessed.

   The facts underneath it are public and unremarkable: `src/starfish.c` binds mangled C++ symbols
   in `libAcbAPI`/`libpf-1.0` from offsets derived by decompilation — the source says so in its own
   comments — and Kodi's `MediaPipelineWebOS.cpp` and mariotaku's `ss4s` bind the same LG symbols in
   the open. What this entry used to do on top of that was name DMCA §1201 and call the practice
   "plausibly the largest single legal exposure in the project": a legal conclusion about the
   author's own conduct, reached by a document with no standing to reach it, sitting in a public
   repository. The open question is worth recording; the verdict was not this document's to render,
   and nobody is obliged to volunteer one against themselves. **Get advice if it matters to you.**
5. **Who owns the copyright?** If there's an employer IP-assignment, the licence choice isn't the
   author's to make.
6. **Audience size.** ACB gone at 5.0 → webOS 4.x only → 2018–2019 models → whose owners must either
   enable Dev Mode (session-limited, apps uninstalled on expiry) or root. Nobody multiplied those
   out. Related: **Plex ships an official webOS app covering webOS 3/4/5** — the same hardware. The
   value proposition against the incumbent is the thing the compliance work has to justify.
7. **Multi-server users.** `auth.rs:392` filters `local && !relay`, falling back from owned to
   *shared* servers, and takes the first match. `stream.rs:239-253` is IPv4-dotted-quad only — no
   DNS and no TLS (it *does* decode chunked bodies; that was listed here as a third limitation and
   never was one). So remote-only servers, relay users, "Require secure
   connections", IPv6 and hostname-addressed servers all dead-end at *"No local Plex server found on
   this network."* There is **no Settings screen and no manual server entry** anywhere in the route
   enum (`app.rs:670-685`).
8. **What should the app do on unsupported firmware?** `requirements.webosRelease` does **not** hide
   the app from a webOS 6 user browsing the channel (§1.4). It needs a graceful failure.
9. **Support model.** A Dev Mode user has no shell, so "send me `/tmp/plxnative-events.log`" doesn't
   work as a bug-report path.
10. ~~**`requiredMemory: 60`** vs a measured ~74 MB peak.~~ **RESOLVED 2026-08-22 — the “~74 MB” was never a measurement.** It was an uncited sentence written into this very section (“things nobody has verified”) and then copied verbatim into five other files, where it reads as established. The real figure, taken on the dev set (M16p3, webOS 4.10.2) from a `features=release` build via `VmHWM`: **35 MB** at boot, **119 MB** browsing Home/detail, **155,292 kB ≈ 152 MiB peak** with playback. Note `VmRSS` on this TV already INCLUDES Mali pages — proven arithmetically, `smaps_rollup` 38,540 kB + `/proc/gpu` 20,159×4 kB = 119,176 kB against `VmRSS` 119,044 kB — so roughly two thirds of the footprint is texture memory and adding `/proc/gpu` on top double-counts. `requiredMemory` is now **160**; it was raised because webOS substitutes a default of **120** when the field is absent or ≤ 0, which made 60 strictly worse than declaring nothing at all.

---

## 7. Ordered path to a public release

**Legal blockers (must precede any public artifact):**

1. ~~Replace `pkg/appfont*.ttf` — Arimo or Inter.~~ **DONE** (Inter, §5.3).
   ~~Remove the Arial blobs from history.~~ **DONE 2026-08-04 for everything public**, and verified
   by cloning the public repo fresh: zero Arial objects reachable from any ref, and a cloner gets
   Inter. `git filter-repo --strip-blobs-with-ids` removed the two Monotype blobs while keeping all
   293 commits — stripping the PATH would have taken the shipping Inter fonts with it, since both
   live at `pkg/appfont*.ttf`.

   **The trap, recorded because it was nearly missed:** rewriting `main` and deleting the branches
   was not enough. The tag `archive/rust-prototype` was also on origin and reached both blobs, so
   for a short window after the repo went public a licensed Monotype font was downloadable from it.
   Deleted. **37 LOCAL refs still carry the blobs** — every agent branch plus that tag — so from
   this clone, never `git push --all` or `git push --tags`.
2. Add `LICENSE` (MIT), and a `Copyright © 2026 <holder>` header — the Plex ToS requires the notice
   *in the source*.
3. Add `THIRD-PARTY-NOTICES.md` + `licenses/` (LGPL-2.1, MIT, Apache-2.0, Zlib, BSD-3-Clause,
   OFL-1.1, Apache-2.0-WITH-LLVM-exception). **Ship them inside the `.ipk`** — LGPL §6's "prominent
   notice with each copy of the work" travels with the binary, not just the GitHub repo. ~200 KB
   against a 4 MB package.
4. ~~Fix the RT icons~~ **DONE 2026-08-02** (§11): providers named in text, verdict-only glyphs, and
   the IMDb/TMDB brand-colour chips — which this list never mentioned — gone with them.
   ~~Attribute `user.svg` / `backspace.svg`~~ **DONE** (`THIRD-PARTY-NOTICES.md` §3).
5. ~~Rename `X-Plex-Product` / `X-Plex-Device-Name`~~ **DONE 2026-08-02** — and the scope was wider
   than these two fields. `account.rs` and `client.rs` had drifted on **five of seven**; both now
   read one `plex/identity.rs`. Three findings this item did not anticipate:
   - **`net.rs` set the libcurl `User-Agent` to `PlexForWebOS/1.0 (LG webOS)`** on every plex.tv and
     `discover.provider.plex.tv` request. Same impersonation as the product string and arguably
     worse — it is what lands in Plex's own server logs — and it named a version that never existed.
   - **`X-Plex-Model` was `49SM9000PLA`**, the author's television, reported as fact by every
     install. Now a generic `LG webOS TV`.
   - **`X-Plex-Version` was two literals** (`1.0` to plex.tv, `0.1.0` to the PMS), already stale in
     opposite directions before release. Now `env!("CARGO_PKG_VERSION")`, with
     `ci/check-package.py` asserting `Cargo.toml` and `appinfo.json` agree.

   **Still open from this item:** the PMS-side `X-Plex-Client-Identifier` is a hardcoded UUID, so
   every install on earth is one device to a server — sessions merge, and the transcode-stop call
   keyed on it can reach across installs. The plex.tv half is already correct (`session.rs` mints
   and persists a v4 UUID); the fix is threading that id into `plex::install`, ~10 lines, and it
   touches the boot path so it wants a player-suite run.
6. ~~Restore the jsmpeg MIT banner; fix `src/svg.c:1`~~ **DONE** (commit 2df3d90).
7. Decide on the Plex ToS licence grant (§5.5 item 3) and write the privacy notice.
8. **NEW:** no source file carries a `Copyright © 2026` header. `LICENSE` landed but §5.5 records
   the Plex ToS obligation as a notice *in the source code*, and that half was never done. One
   banner in `src/main.c` and `rust-modules/src/lib.rs`.

**Privacy/security — all but two DONE 2026-08-02:**

9. ~~`--remap-path-prefix`~~ **DONE.** See §4 item 2 for the corrected numbers and why this was the
   hard release blocker: `ci/check-elf.sh` already *gated* on host paths while nothing set the flag,
   so the first tagged release would have failed its own check. The gate had two bugs of its own —
   its hint named `-C --remap-path-prefix` (it is a top-level rustc flag) and its `/home/[a-z]`
   pattern matched plex.tv's `/api/v2/home/users` endpoint, which would have failed **every** build
   regardless. Both fixed, and the host-path assertion was split out of the config-dependent ones so
   it runs on a dev machine too — it had never once executed against a real build.
   ~~Replace the control-file Maintainer~~ **DONE**, plus `Homepage`/`License`, all three asserted.
10. ~~Put the whole `/tmp/plxnative-*` surface behind a cargo feature~~ **DONE.** New `devtriggers`
    feature (a *second* feature, not more things under `devtools`, whose stated contract is
    draw-only — that promise is worth keeping) and a new `src/dev.rs` every read goes through.
    Verified on the built ARM binary: a `RELEASE=1` build's only `/tmp` strings are its three log
    sinks. The FIFO path and the capture trigger are optimised away entirely.

    Two things this item's framing would have missed. **`automated_boot` names no path** — it
    `read_dir`s `/tmp` and matches by prefix, so a literal-replacement sweep greps clean while the
    boot screen is still steerable by a squatted file. And **the euid check is now moot**: nothing
    has to survive, so there is no trigger left to `stat`.

    The assumed conflict does not exist — `make run` touches `/tmp` only to clear and read back the
    event log, and `tests/run.py` builds with plain `make`, so the harness never sees a release
    binary and gating the whole surface costs it nothing.
11. ~~`setrlimit(RLIMIT_CORE, 0)`~~ **DONE** (`src/main.c`, the other half of the tracer's re-raise).
    `make DEBUG=1` keeps cores via a new `-DPLX_DEBUG`; that is now the only behavioural thing
    `DEBUG=1` changes.
12. Sanitize `tests/manifest.json` and the personal-library references in `docs/`. *(Narrower than
    it reads: `docs/` is ~12 lines across 5 files. The concentration is `tests/` — the inventory
    appears twice, in `manifest.json` and `README.md` — plus, unlisted here, real titles in
    `rust-modules/src` comments, most of which are the RATIONALE for the code and should stay.)*
13. Rotate the Plex token. Clean `main.o`, `.claude/worktrees/`, `.tv-*.log`, and the stale 200 MB
    `core` sitting in the app dir on the dev TV.
    **The worktree half was structural, not housekeeping:** 16 copies of `config.local.h` with the
    live token were held out of the index by `.git/info/exclude` alone — local-only, not reviewable,
    absent from a clone, and inert in a CI checkout. Moved to `.gitignore` 2026-08-02.

**Then packaging:**

13. ~~**Resolve the app directory at runtime**~~ **DONE** (commit a6864dd) — `paths.rs`, via
    `/proc/self/exe`, with the session path a probed search order and `save()` logging its failure.
14. ~~Run `webosbrew-ipk-verify --details --fw-releases '>=4.0, <5.0'` locally until clean.~~
    **DONE 2026-08-02 — clean on 4.4.2 and 4.10.0.** Found and fixed two bugs that made the ipk
    uninstallable; see §9. A real ipk install on the device is now proven end to end.
15. ~~Add `Installed-Size`~~ (done, §9) / ~~add `Homepage` / `License`~~ **DONE 2026-08-02, and
    asserted** / ~~strip the binary~~ **DONE**: measured 7,835,376 → 5,495,476 (−2.34 MB, 30%),
    `Installed-Size` 10117 → 7781 KiB, ipk 4.85 MB. Only the **staged** copy is stripped, never
    `pkg/plxnative` — `tools/crash-report.sh` symbolizes against that local binary *and* md5-compares
    it to the on-TV copy, so stripping in place would break the identity check and lose function
    names from every release crash report. ~~make the ipk reproducible~~ (done — byte-identical
    rebuilds, re-verified after the strip landed).
16. ~~Draw real icons (they're solid gold squares today).~~ **DONE 2026-08-02** — `tools/mkicons.py`
    cuts all four sizes (80/130 for LG, 160/320 for the channel listing) from
    `assets/logo-master.png`, measuring the master's ink and refitting it to LG's panel geometry
    rather than scaling the master's own canvas. Verified in the real launcher; see §10.
17. ~~Write a root README with build instructions that work from a clean clone.~~ **DONE 2026-08-02.**
18. `webosbrew-gen-manifest`, attach both assets to a GitHub Release, submit the PR, tick the AI-use
    box that says an experienced developer reviewed and tested the generated code.

**19. NEW — the ordering trap, and the things only a human can do.** These are what actually stand
between here and a release, and none of them is code:

- **Merge `publish-prep` to `main` BEFORE tagging.** `main` today has no `.github/`, no `LICENSE`,
  no `licenses/`, no `icon160.png`/`icon320.png`. The generated manifest pins `iconUri` to
  `…/main/pkg/icon160.png`, so a tag cut before the merge publishes a manifest whose icon URL 404s
  — and webosbrew's own `repogen.lintpkg` rejects that.
- **Make the repo public**, for the same reason: that raw URL must return 200.
- ~~Set the `RUST_NIGHTLY` repo variable~~ **DONE 2026-08-02** — `nightly-2026-07-02`, the toolchain
  the device-verified builds actually used. Unset, both workflows fell back to floating `nightly`,
  which defeats the pin `-Z build-std` needs (and a `rust-toolchain.toml` cannot substitute — see
  the Makefile).
- **Run `ci.yml` by `workflow_dispatch` before ever cutting a tag.** Nothing in `.github/` has ever
  executed: no CI run, no Release, no tag. The NDK action's cache and relocate step, the toolbox
  `.deb` download, the pip install, the arm64 runner — all unexecuted. The first run should not be
  the release path.
- **Write `packages/com.beb.plxnative.yml` in a fork of `webosbrew/apps-repo`** and open the PR.
  **DRAFTED — `docs/webosbrew-package.yml` is the ready file**; copy it into the fork under that
  name. Schema re-verified against upstream 2026-08-04: required keys are `title`, `iconUri`,
  `manifestUrl`, `category`, `pool`, `description`; `shortDescription` is capped at 80 characters;
  `category: multimedia` and `pool: main` match what comparable clients use.

  Carry `requirements.webosRelease: '>=4.0, <5.0'` — **that bound exists ONLY in this hand-written
  file**, not in the manifest CI generates, so without it a webOS 5+ user installs an app whose
  `libAcbAPI` and `libav*.so.57` are absent. The value is not a guess: `repogen/check_compat.py`
  passes the string **verbatim** to `webosbrew-ipk-verify --fw-releases`, which is the same tool
  and flag §9 already ran against the real binary. Note it is `<5.0`, not `<4.9` — webOS numbers
  **4.10 above 4.9**, and 4.10.0 is one of the two firmwares the binary is proven good on.

  **The PR template carries an AI-use declaration. ANSWERED 2026-08-04:** the box is *"AI coding
  agents were used; an experienced developer has reviewed and tested all generated code."* The
  repository states outright that apps "made primarily by AI and submitted without meaningful human
  development, testing, and review are not accepted", and that the submitter remains responsible
  for the code either way — so the second half of that box is the load-bearing part, and Gleb
  confirms it: every change in this project was reviewed and exercised on his own television. It is
  a visible pattern in the record rather than an assurance — the scrubber's credits band, the
  on-screen frame counter leaking into the screenshots, a wrong RAM figure and an unexplained uid
  were all caught by him reading the output, not by the agents producing it.

  Two other checkboxes on the same template: tested on a real webOS TV (yes — `tests/run.py` is
  21 cases against a live PMS on the device), and complies with the repository rules (§1.5).

---

## 7a. Cutting a release (2026-08-04)

**Actions → Release → Run workflow → pick `patch`, `minor` or `major` → Run.** That is the whole
process. Nothing is edited by hand and nothing is tagged by hand.

The `prepare` job runs `ci/bump-version.py`, which is the ONE place that knows the version lives in
four files — `pkg/appinfo.json` (the source), `ipkroot/ctl/control`, `rust-modules/Cargo.toml` and
`Cargo.lock`. It then runs `ci/check-package.py` to prove they agree **before** committing, commits
`release: X.Y.Z`, tags `vX.Y.Z`, pushes both, and hands the tag to the build.

Two other ways in, both still supported:

- **`version: 1.2.3`** — an explicit number instead of a bump level.
- **`rebuild_tag: v0.1.0`** — re-publish an existing tag, skipping the bump entirely. This is the
  input the old workflow called `tag`, and it was *required*, which is why the first dispatch
  failed: it was given `0.1.0`, no such tag existed, and checkout died fetching `refs/tags/0.1.0*`.
  It is optional now and named for what it does.

**Why this shape, and the two traps in it.**

*No double build.* `prepare` pushes the tag with `GITHUB_TOKEN`, and GitHub deliberately refuses to
start a workflow run from a `GITHUB_TOKEN` push. So the `push: tags: ['v*']` trigger stays for a
hand-pushed tag without firing a second time on our own. If that push is ever moved to a PAT or a
deploy key, this becomes an infinite loop — that is the thing to remember.

*`needs` exposes only DIRECT dependencies.* `legal-gate`, `build` and `publish` cannot see
`prepare`, so the resolved tag is computed once in `guard` and published as `needs.guard.outputs.tag`;
those jobs list `guard` in their `needs` purely to read it. Repeating the
`prepare || rebuild || ref_name` fallback in each job silently evaluates to empty in three of them.

*Backwards is refused; sideways is not.* The first attempt at this got the reference point wrong
and made the FIRST RELEASE impossible: it required the new version to exceed `pkg/appinfo.json`,
which already said `0.1.0` when nothing had ever been tagged, so `version: 0.1.0` — the correct
input for a first release — was rejected as "not greater than the current 0.1.0". The thing that
must never repeat is a **tag**, not a number in a file, so that is what `prepare` now checks
(`git rev-parse -q --verify refs/tags/vX.Y.Z`), and it says which input to use instead. An
unchanged version simply commits nothing and tags `main` as it stands.

Only going *backwards* is refused, and there the original reasoning holds: `releases/latest`
resolves by publish DATE while the Homebrew Channel compares VERSIONS to decide an update exists,
so a backwards release is one no installed client will ever offer.

**Locally**, `ci/bump-version.py --current` prints the version and `ci/bump-version.py patch
--dry-run` shows what would move. Prefer the workflow for anything that ships: a local bump has to
be committed, tagged and pushed by hand, which is the sequence this exists to remove.

---

## 7b. A release is always `com.beb.plxnative` — and a flavoured artifact must never become one

Since 2026-08-21 this tree can package two ids. `com.beb.plxnative` is the app users install: it is
the id in every manifest, every channel listing, every release asset name and every `ipk.sha256`.
`com.beb.plxnative.debug` is the developer build that lives beside it on one television, and it has
no release, no published hash and no listing — ever. `docs/two-installs.md` is the full account;
what matters here is that the packaging path knows the difference and says so.

`make ipk` defaults to `FLAVOR=debug`, so **the release build has to name the flavour**:

```sh
make FLAVOR=stable RELEASE=1 ipk      # the only combination that produces a shippable artifact
```

Four mechanisms keep the two apart, and the reason for four is that each covers a hole the
others do not:

- **`release-guard`** refuses `deploy`/`ipk` on `FLAVOR=stable` without `RELEASE=1`, and names
  `ALLOW_DEV_ON_STABLE=1` as the deliberate hatch. Before the flavour split, shipping a dev build
  under the released id could only happen by publishing by hand — which is how v0.2.1's defects got
  out; now it is one forgotten `RELEASE=1` on a machine that also has a television, so it gets a
  mechanism rather than a rule.
- **`ci/check-package.py` grades the same thing on the packaged BYTES**, which is the half that
  survives somebody reaching for the hatch and forgetting: the stable package must not contain the
  `plxnative-noidle` dev witness. It also derives the packaged id from the staged
  `applications/<dir>` name rather than from the tracked `pkg/appinfo.json`, because assuming the
  stable id would make every path below it grade a debug package **vacuously** — an empty `rglob`
  prints nothing, fails nothing, and reports success.
- **The stable descriptor transform is asserted to be the IDENTITY** (`ci/flavor.py --selftest`, run
  by `make check`). Nothing about the second id may perturb the released artifact's bytes; if it
  did, the sha256 in every published manifest would be wrong, and that hash is the entire integrity
  story here because nothing is code-signed.
- **`make ipk` writes `pkg/ipk.sha256` only for `stable`.** That filename is a released asset name;
  a flavoured package writing it would replace the hash a release note's own verification command
  tells people to check.

Installing and removing the second app on the television is `appInstallService`'s dev pair — the
same route a Dev Mode user's tooling takes, and the reason `make deploy` alone cannot create an
app (SAM has to learn the id, and the LS2 role file that permits `com.webos.media.*` is written by
the installer, §3.5):

```sh
make FLAVOR=debug install     # ipk -> dev/install -> deploy into it
make FLAVOR=debug uninstall   # dev/remove; refuses the stable id
```

which are these — wrapped by the Makefile in `script -qc "…" /dev/null`, because `luna-send`
needs a tty over plain ssh, and given `-i` because the subscription is the only place
`appinstalld` ever names a failure:

```sh
luna-send -i -a com.webos.appInstallService luna://com.webos.appInstallService/dev/install \
  '{"id":"com.beb.plxnative.debug","ipkUrl":"/tmp/com.beb.plxnative.debug_<version>_arm.ipk","subscribe":true}'
luna-send -i -a com.webos.appInstallService luna://com.webos.appInstallService/dev/remove \
  '{"id":"com.beb.plxnative.debug","subscribe":true}'
```

`install` **deploys afterwards, deliberately**: `appinstalld` replaces `applications/<id>/`
wholesale, so stopping at the install leaves the packaged binary behind and you are looking at a
build you did not make.

---

## 8. Release CI (built 2026-08-01)

`.github/workflows/{ci,release}.yml` + `.github/actions/webos-ndk` + `ci/`.

**The runner is forced.** `webosbrew/native-toolchain` publishes exactly three host builds for
`webos-d7ed7ee.6` — `darwin-arm64`, `darwin-x86_64`, `linux-aarch64` — and **no linux-x86_64**.
So the cross-build runs on **`ubuntu-24.04-arm`**. The old hardcoded `darwin-$(uname -m)` URL
*resolved* on ubuntu-latest and quietly delivered a Mach-O toolchain that passed `test -x` and died
much later with `Exec format error`; `make setup-env` now refuses that host by name. CI pins the
**same NDK release the dev Mac uses** (sha256 `45a2d12f…`, verified 2026-08-01), and its sysroot was
confirmed to carry all nine `LIBS_REAL` including `libAcbAPI`/`libplayerAPIs`/`libpf-1.0`.

**`ci.yml`** (push/PR) — two parallel jobs, mirroring the Makefile's own rule that `make check` is
not a prerequisite of `all`, so a red clippy can't mask a broken ARM link:
- `host-checks` (ubuntu-latest): `make check`, and prints the real test count into the job summary
  rather than trusting CLAUDE.md's (which has gone stale twice — it says 59; the suite is ~282).
- `cross-build` (ubuntu-24.04-arm): build → `make ipk` → `ci/check-elf.sh` → `ci/check-package.py`
  → `tools/font-hint-audit.py` → **`webosbrew-ipk-verify`**.

**`release.yml`** (tag push) — `guard` (tag must equal `appinfo.json`'s version) → `legal-gate` →
`build` → `publish`. Only `publish` gets `contents: write`.

**The assertions, and what each exists for:**

| Check | Catches |
| --- | --- |
| `webosbrew-ipk-verify --fw-releases '>=4.0, <5.0'` | The gate webosbrew's own PR CI runs. Resolves `DT_NEEDED` + symbols against 14 firmware databases — the exact error class the stub-`.so` trick makes invisible at link time. Fully offline. **Never been run on the real binary.** |
| `ci/expected-dt-needed.txt` diff | Dependency drift. Calling into any library that happens to be in the NDK sysroot silently adds a `DT_NEEDED` entry and the link still succeeds. |
| CP15 barrier scan | The SIGILL regression. `.cargo/config.toml` names the exact scenario — "a future CI" setting `RUSTFLAGS`, which *replaces* the config list and drops `target-cpu=cortex-a9`. Carries a positive control (`dmb` count > 100) so a zero can't pass vacuously. |
| build-host identity | The dev's LAN IP and `/Users/…` paths. A clean checkout has no `config.local.h`, so `app.h`'s `__has_include` falls back to `YOUR_PMS_HOST` — the CI artifact is leak-free *by construction*. Skipped on a dev machine, where `config.local.h` exists by design. |
| `legal-gate` | Refuses to publish without `LICENSE` + `THIRD-PARTY-NOTICES.md`, and **hard-fails if `pkg/appfont*.ttf` is still Arial**. (`check-package.py` treats Arial as an XFAIL so the gate could land before the swap; the release job does not.) |
| manifest sha256 vs artifact | The hash is enforced **on the device** at install and by nothing in CI. A mismatch is invisible until it breaks every user's install. |

**Reproducible ipk — done and verified.** `tar czf` was embedding `gleblinnik/staff`, the current
mtime, a gzip header timestamp and readdir order into every shipped archive. `ci/mkipk.py` builds
both members deterministically (Python `tarfile` rather than tar flags, because GNU tar and bsdtar
disagree irreconcilably on `--owner`/`--uid`/`--sort`), and `ar` gained `D`. Two consecutive
`make ipk` runs now produce byte-identical `a682acb1…`. That matters because the manifest carries
that hash and there is no code signing anywhere in the chain — without reproducibility, "rebuilt"
and "tampered with" are indistinguishable.

**`RELEASE=1` — no dev chrome in a public build.** `rust-modules/Cargo.toml` gained a `devtools`
feature, on by default so every ordinary `make`, `make test` and harness run behaves exactly as
before. `make RELEASE=1` passes `--no-default-features`, which today removes the **on-screen
seven-segment counter** (`app.rs`). The fps scenes are unaffected — they grade the once/sec
heartbeat in the *event log*, never the pixels. Anything added to this feature must be draw-only:
the device is the only test this project has, so a release build must not differ from the tested
one in any way that could change behaviour, only in what it paints.

Three traps this cost, all found by measurement rather than reasoning, all silent:

1. **Both feature sets wrote the same `libplxnative_modules.a`.** Cargo fingerprints the build but
   does not hash its output, so after a `RELEASE=1` build it reports the dev build
   *"Finished in 0.04s"* and leaves the release `.a` in place — which `make` then links with no
   comment. Fixed by giving each configuration its own `--target-dir`.
2. **A stamp file could not drive the relink.** macOS ships GNU make **3.81**, which (a) decides a
   target's up-to-dateness from a stat taken *before* its prerequisites' recipes run, and (b)
   compares mtimes at **one-second granularity** — a stamp written 0.5 s after the binary compares
   *equal*. Both were observed directly. So the config check runs at **parse** time and **deletes
   `pkg/plxnative`** when the configuration changes; "the target does not exist" is not a timestamp
   comparison and cannot be defeated by either.
3. **`make RELEASE=1 && make deploy` deploys a DEV binary** — the second invocation has no
   `RELEASE`, so it rebuilds and ships that. The flag must be on *every* invocation that produces
   or ships the binary (`make RELEASE=1 deploy`). `deploy` and `ipk` now echo which configuration
   they are shipping, and `release.yml` asserts `pkg/.build-config` really says
   `--no-default-features` rather than trusting that the flag took.

Verified on the device, not just by binary size: the release build deployed to the TV
(md5-matched) renders the who's-watching screen with no counter; the dev build renders `62`.

**Version single-sourcing.** `pkg/appinfo.json` is now the one place the version is written; the
Makefile derives `IPK_VERSION` and the ipk filename from it, `check-package.py` asserts
`ipkroot/ctl/control` agrees, and `release.yml`'s `guard` asserts the git tag does too.

**Nightly pinning.** A `rust-toolchain.toml` would **not** work: `cargo +nightly` on the command
line outranks the file. The pin therefore comes through a new `RUST_NIGHTLY ?= nightly` Makefile
variable — dev machines behave exactly as before, and CI passes `RUST_NIGHTLY=nightly-YYYY-MM-DD`
(set the `RUST_NIGHTLY` repo variable). `-Z build-std` recompiles std from source, which is the
most drift-sensitive thing this build does.

**Two traps encoded in `release.yml`, both of which fail silently:**
1. `gen-manifest` writes a **bare filename** into `ipkUrl`, resolved relative to the manifest URL —
   so the `.ipk` and the manifest must be assets of the *same* release.
2. **A draft or prerelease is excluded from `releases/latest`**, so publishing one leaves users
   resolving the *previous* manifest — an update that silently never arrives. Both flags are
   pinned false.

**Propagation:** webosbrew has no webhook. The registry rebuilds on `cron: 41 */3 * * *` and
re-fetches every manifest, so expect **~1.5–3 h** plus a ~10 min CDN TTL. The Homebrew Channel
compares versions by **plain string equality** — its `versionHigher()` helper is dead code.

**Not built: on-device CI.** It is the only real gate, but `tests/run.py` has **no mutual-exclusion
lock** (no `flock`, no pidfile), and there is one television — a scheduled run overlapping with the
developer at their desk produces failures that look like player regressions and are not. It would
also need a live PMS token as a repo secret, and it mutates shared state (real watch history, real
`/:/unscrobble` calls). If added: `schedule` + `workflow_dispatch` only, never `pull_request`, with
a device-side `flock` and a Wake-on-LAN probe that reports "TV asleep" distinctly from a test
failure.

---

## 9. The ipk was never installable (found 2026-08-02, fixed)

§6's "five-minute check" was the highest-value item in the document, because running it found that
**every `.ipk` this repo has ever built fails to install** — two independent bugs, both invisible to
every check that existed.

They were invisible for one structural reason: **the dev loop never installs the package.** `make
deploy` scp's a binary into an app directory the TV already has registered, and `make test` drives
that. So the `.ipk` — the only artifact a user would ever receive — had no test at any tier, on a
project whose whole verification philosophy is that the device is the real gate.

**Bug 1 — no package descriptor.** webOS wants two descriptors, and they are not the same file:
`usr/palm/applications/<id>/appinfo.json` (the *application*) and
`usr/palm/packages/<id>/packageinfo.json` (the *package*, which tells `appinstalld` what app ids the
package owns). Ours had only the first. Confirmed against the device: all 12 store apps and **both
Homebrew Channel apps** on the dev TV carry a `usr/palm/packages/` entry — `com.beb.plxnative` was
the only application on the box without one, precisely because it was scp'd rather than installed.
`webosbrew-ipk-verify` opens that file first and reports the miss as

    Failed to open com.beb.plxnative_0.1.0_arm.ipk: No such file or directory (os error 2)

— the *ipk's* name, not the member's, so it reads like a corrupt or missing archive rather than a
missing member. `ci/mkipk.py` now synthesises it from `pkg/appinfo.json`, keeping the version
single-sourced.

**Bug 2 — GNU `ar` writes member names LG will not read.** With the descriptor added the verifier
went green on both 4.x firmwares — and the TV still refused the package:

    AppInstallD TASK_ERROR {"app_id":"com.beb.plxnative","error_code":-5,
                            "error_text":"Failed to extract package"}

GNU `ar` terminates short member names with `/`, so `ar t` on our own ipk showed `debian-binary/`,
`control.tar.gz/`, `data.tar.gz/`. `appinstalld` looks the members up by exact name and fails the
package before unpacking anything. `dpkg-deb` and `ares-package` both write the bare names. The
Makefile's comment — *"the ipk uses the NDK's `ar` (GNU format; macOS BSD `ar` won't work)"* — had
it backwards: GNU format is the thing that breaks, and BSD `ar` would have produced a **working**
ipk. `ci/mkipk.py` now writes the 60-byte `ar` headers itself, which also removes the NDK from the
packaging path entirely (`make ipk` runs on any host with Python).

**Neither bug is reachable from the other's side.** `webosbrew-ipk-verify` parses a GNU-named
archive without complaint, so the submission gate would have passed an ipk no TV could install; and
the TV's extractor never gets far enough to notice a missing descriptor. Only running both found
both.

**End-to-end proof, on the device.** Old app directory removed, `RELEASE=1` ipk installed through
`luna://com.webos.appInstallService/dev/install`, which reported `changeReason: appInstalled` and
registered a launch point. The installed build then launched and: resolved its fonts from the
install prefix (`init_text ok=1`, **zero** `FONT FALLBACK` lines — the §3.5 `paths.rs` fix working
under a real install rather than an scp), read the persisted session, rendered the who's-watching
picker, and drew **no dev counter**, confirming `RELEASE=1` in the shipping artifact.

**Regressions locked in.** `ci/check-package.py` now asserts the payload carries
`usr/palm/packages/<id>/packageinfo.json`, and parses the `ar` headers to assert the members are
exactly `debian-binary`, `control.tar.gz`, `data.tar.gz` — bare and in that order. Reproducibility
survives the rewrite (two consecutive `make RELEASE=1 ipk` runs agree byte for byte).
`Installed-Size` is now written from the real payload (8568 KiB), replacing its absence.

**Cost of the miss, had it shipped:** the manifest's sha256 would have matched, the download would
have succeeded, and every install would have failed at extraction — on a channel where the
developer has no shell on the user's TV and `/tmp/plxnative-events.log` is unreachable (§6.9).

---

## 10. App icons (built 2026-08-02, verified in the launcher)

`tools/mkicons.py` cuts `pkg/icon.png` (80), `pkg/largeIcon.png` (130) and the channel listing's
`icon160.png` / `icon320.png` from one square master (`assets/logo-master.png`). It measures the
master's **ink** and scales until that ink lands where LG wants it, because the geometry is not
"the logo, scaled": the guide specifies a **126×126 background panel** with the logo inside
**115×115 plus ≥5 px padding**, so a master's own canvas margins may be off by any amount and a
plain four-size export would inherit it. Emitted padding is 9 px at 80 and 14 px at 130.

**It scales the whole master and crops — it does not cut the logo out and paste it onto a flat
panel.** The pasting version worked only because the first master's background was pure `#000`
everywhere. A master with a vignette, glow or gradient carries those pixels *inside* the ink bbox
and not outside it, so pasting stamps a faintly-wrong rectangle of background into the tile: the
same class of seam as a mismatched `iconColor`, and harder to catch because it is a few levels
rather than a colour. The v2 master has exactly that (a glow around the orange X reaching rgb
37,20,20 against a 6,6,6 field), which is what forced the rewrite. Scaling the whole canvas keeps
the background continuous by construction and costs nothing when the background is flat.

Two decisions were measured rather than judged, against the TV's own `115x115` store-icon cache
(`/media/cryptofs/apps/usr/palm/applications/*/`) — which is what the launcher actually draws:

- **Real tiles are full-bleed opaque squares.** Apple TV and Spotify are on black, Netflix and
  YouTube on white; the launcher supplies the rounded corners. A dark tile is on-style.
- **The logo spans 70–91 % of tile width** (Apple TV 70, Spotify 74, Netflix 80, YouTube 91). We
  take 78 %.

**A stacked lockup loses its second line, and the script says so before you ship it.** The v1
master was two lines; the lower one was 9.6 % of the ink height, which renders **~4 px tall at 130
and ~2.5 px at 80** — mush at the first size, absent at the second. Every reference icon with
readable text carries **one** line at aspect 3.1–4.2; the only 2:1 reference (Apple TV) carries no
small text at all. So `mkicons.py` prints each band's projected height at 130 and flags anything
under 8 px, and `--band=N` keeps just one. The **v2 master (shipped) is single-line at aspect
2.68**, so the question no longer arises.

**`iconColor` must match the icon's own background, and for months it didn't.** It was `#e5a00d`,
so the launcher painted the tile Plex gold and drew our black icon on top — a black rectangle
floating in a gold tile, with a hard seam neither HBO Max nor Twitch has, because *their* icon
background equals *their* `iconColor`. Nothing in any file was wrong; the defect existed only once
the system composited the two, which is why a device screenshot found it and no amount of file
inspection would have. Now `mkicons.py` writes `iconColor` from the master's own corner pixel
(`#060606` for v2) and `ci/check-package.py` asserts the two agree within 2 levels — verified to
fail when deliberately reverted to the gold. This also retires the trademark note above: `#E5A00D`
was Plex's exact brand gold, and nothing in the shipped package carries it now.

### 10.1 The icon is NOT off-centre — LG puts every icon there (settled 2026-08-02)

It looks 5 px high. It is, and so is Netflix's. `AppTile.qml:73` in the TV's own launcher
(`/usr/palm/applications/com.webos.app.home/qml/Containers/Main/`) reads:

```qml
width:  style.tile.getAppIconSize(entry.tileType)   // 115 for every app tile
anchors.centerIn: parent
anchors.verticalCenterOffset: -5                     // <- the whole effect, a bare literal
```

Unconditional; the only per-app input on that path is *which file* is loaded
(`mediumLargeIcon || largeIcon || icon`), never geometry. Template-matching the store-delivered
115×115 bitmaps into the captures puts **all 16 store apps and us on the same row** — 913 unfocused,
893 focused. The extra asymmetry is `ribbon.hiddenHeight: 22` running the bottom of every 252 px
tile off the panel; the sign of the apparent offset **flips with focus** (≈4.5 px high focused,
≈5.5 px low unfocused), which is itself the proof it is a screen-edge artifact.

Two corrections worth keeping, because both were mine:
- **The "9.5 px" I first measured was wrong.** I took the tile's *bounding-box* top (840) instead
  of its local top at the icon's own column (830) — the tile is a parallelogram sheared 1/6 px per
  row, and over the 115 px icon that bbox sits ~9.6 px high. Real figure: LG's 5.
- **`largeIcon` at 130×130 is correct** and is what LG's own built-ins ship. Every icon is rendered
  into a 115×115 box regardless of source; 192→115 and 80→115 land on the same row.

**Why it was legible on our tile and nobody else's:** every opaque icon on this TV has its artwork
background *exactly* equal to its declared `iconColor` (delta 0 across 14 apps), so their 115 box is
invisible and only the logo reads. Ours disagreed — gold `iconColor`, black artwork — so our box was
a visible black rectangle, and a visible box makes a 5 px offset legible. That was the real defect,
and it is the one already fixed above.

Open, small, untested: `mediumLargeIcon` is a real appinfo key (`/usr/palm/applications/airplay/`
declares it). Supplying a native **115×115** cut there would replace a Qt 0.885 resample — done on
every draw, since `cache: false` — with a 1:1 blit. Zero positional change, marginally crisper.
Unknown whether SAM honours it for a dev-mode install.

### 10.2 Splash (device-verified 2026-08-02)

`splashBackground: splash.png`, 1920×1080, cut by `tools/mkicons.py --splash`. **It works for
`type: "native"`** — which was the real doubt, since LG documents the field in a web-app context;
Amazon, Apple TV, Netflix and YouTube on this TV are all native and all declare one, and all four
are exactly 1920×1080. Captured on the panel: full-bleed, no letterbox, no crop, splash up before
the first sample and handing straight to the UI with no black frame at 2.8 s sampling.

LG's "the splash screen should not be black" line is not enforced — **Apple TV's own splash is
99.5 % pure black**; ours is 66 %.

The master arrives at 1672×941, so the script resamples (LANCZOS, 1.148×). Measured cost: none
detectable — 1 px 10–90 % edge transition and full contrast on both sides, because the art is
vector-derived with hard edges. It refuses a master that is not 16:9 rather than letterboxing.

### 10.3 The badged tile — the second install's artwork (2026-08-21, not yet seen on a panel)

A developer build now lives beside the released app on one television (`docs/two-installs.md`), and
the two tiles sit side by side in the launcher. `pkg/dev/icon.png` and `pkg/dev/largeIcon.png` are
tracked, cut from the SAME master by the same script:

```sh
python3 tools/mkicons.py assets/logo-master.png --out-dir=pkg/dev --sizes=80,130 --badge=DEV
```

The badge is a **full-bleed bottom bar** — amber (`theme::RESUME_FILL` over `AMBER_950` ink, the
design system's own filled-control pair, so it is on-brand while being the one thing on the tile
that could not be mistaken for the release artwork). Both halves of "full-bleed bottom bar" are
load-bearing rather than taste:

- **It must not touch pixel (1,1).** `iconColor` paints the launcher tile *behind* the icon, and
  `ci/check-package.py` asserts it agrees with the icon's own corner pixel within 2 levels — the
  gate §10 exists to describe, added because a gold tile shipped under a black icon for months and
  was invisible in every file, since the defect only exists once the system composites. A corner
  ribbon or dot would move that pixel by ~240 levels and fail the check; a bottom bar leaves the
  corner alone, so **one `iconColor` stays correct for both flavours and the badge needs no
  descriptor change at all**. That is why the flavour transform moves only `id` and `title`. It is
  enforced, not merely documented: `check-package.py` runs the same pixel-(1,1)-within-2-levels test
  a second time against `pkg/dev/largeIcon.png`, so a badge that creeps into the corner fails the
  package rather than shipping a hard-edged rectangle in a differently-coloured tile.
- **A bar, not a whole-tile tint.** Tinting means moving `iconColor` in lockstep — or reproducing
  exactly the defect above — and it stops looking like the product.
- **Rasterized natively at each size**, never scaled from one master: at 80 px the bar is 18 px and
  the glyphs about 10, and a 4x downsample of either is a smear where a stroke drawn at 80 is a
  stroke. Same reasoning as §10's `--band` height floor.
- The ink colour is **derived** from the fill by WCAG relative luminance rather than fixed, so a
  different `--badge-fill` cannot silently produce grey-on-grey — a tile nobody can read, which is
  not an error anywhere.

`mkicons.py` now **rejects unknown flags** instead of ignoring them, and that is the point of the
change rather than tidiness: the old parser filtered out everything starting with `--` and read the
four options it knew, so a typo (`--outdir=pkg/dev`) silently wrote the BADGED set over
`pkg/icon.png`, `pkg/largeIcon.png` and `pkg/icon160.png` — the last of which `release.yml`
publishes as a raw.githubusercontent URL for the channel listing. One mistyped letter and the
artwork everyone sees before installing carries a DEV bar.

The flavoured `.ipk` stages `pkg/dev/icon.png` over the basename `icon.png`, so `appinfo.json`'s
icon fields and `ci/check-package.py`'s by-basename payload grading are unchanged: **the flavour
lives in the directory a file is read from, never in the name it is packaged under.**

Unverified: nobody has looked at the two tiles on a panel yet — legibility of the bar at the 115x115
box the launcher actually draws into is `docs/two-installs.md` §6.5.

---

## Sources

webosbrew: `github.com/webosbrew/apps-repo` (README, `repogen/`, `content/schemas/`,
`.github/workflows/`), `github.com/webosbrew/webos-homebrew-channel`,
`github.com/webosbrew/dev-toolbox-cli`, `github.com/webosbrew/native-toolchain`,
`github.com/webosbrew/webos-bridge-64to32`, `repo.webosbrew.org/api/apps.json`, `webosbrew.org/devmode`.
LG: `webostv.developer.lge.com` (appinfo-json reference, app-approval-process, app-ecosystem,
guides/flutter-for-webos, news 2026-06-30, web-api-and-web-engine), `forum.webostv.developer.lge.com`
(threads 5262, 10290, 3365, 10880, 27101), `github.com/lg-flutter-webos/ndk`.
Other native webOS apps: `xbmc/xbmc` `docs/README.webOS.md`, `mariotaku/moonlight-tv`,
`webosbrew/retroarch-cores`, `throwaway96/faultmanager-autoroot`.
Licences: `gnu.org` LGPL-2.1 + GPL FAQ, `ffmpeg.org/legal.html`, `learn.microsoft.com` font-redistribution FAQ,
`libsdl.org`, `freetype.org`, `libjpeg-turbo.org`, `crates.io`, `rust-lang/rust` COPYRIGHT.
Plex: `plex.tv/about/privacy-legal/plex-terms-of-service/`,
`plex.tv/about/privacy-legal/plex-trademarks-and-guidelines/`, `developer.plex.tv`, forum announcement 2025-09-15.
Third-party marks: Rotten Tomatoes Developer Network (archive 2025-02-19), Fandango Data Feed Terms
(archive 2024-11-05), `themoviedb.org/api-terms-of-use` (rev. 2023-10-20).
On-device evidence: `.abi-cache/libavcodec.so.57.89.100` build-config string; NDK `readelf -d pkg/plxnative`;
fontTools dumps of `pkg/appfont*.ttf`; `tar tzf ipkroot/data.tar.gz`.

---

## 11. The rating marks are ours now (2026-08-02)

The last hard trademark blocker. Rotten Tomatoes' marks had **no licensing route in existence** —
their developer programme is closed to unofficial projects and `developer.fandango.com` does not
resolve, so there was not even an application path — and redrawing a mark is the standard
infringement pattern rather than a defence.

Removed: the 5 RT states as 11 layered SVGs (fruit, Certified Fresh seal, upright and spilled
popcorn tubs), plus a dead `tmdb.svg` and a retired `star.svg` that nothing drew. Also removed —
and this was not on the original blocker list — the IMDb and TMDB **logotype chips**, which
reproduced two more brands' wordmarks in their exact brand colours to answer "whose score is this?".

Replaced by naming each provider in words (referential use needs no licence) and drawing only the
VERDICT: our own tomato — red ripe, gold for the rarer Certified bar, hollow when drained — and a
two-figure crowd for the audience score, green or drained. Four assets instead of eleven.

The design brief is `Details Screen.dc.html` in the owner's Claude Design project. Verified on
device against the live PMS: `rk=1` (ripe + upright, all four providers) and `rk=2020` (**rotten** +
upright). Rendered ink matches the design's geometry within ~1.5 px.

**Two states could not be verified on device**: `certified` and `spilled` appear on no item in this
library. Both reuse masks the verified states already exercise and differ only by a tint constant
(`RATING_CERTIFIED`, `RATING_MUTED`), so the risk is a wrong colour, not a wrong drawing.

What remains is not a licensing question but a taste one: the tomato is a fruit, and a fruit next to
a percentage in a movie app still gestures at Rotten Tomatoes. That is the owner's call, made with
the facts on the table, and the two shapes a rights holder would actually write about — the seal and
the popcorn pair — are the ones that are gone.
