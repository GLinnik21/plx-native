# player/ — the buffer-feed video engine

This is the in-process **buffer-feed** playback engine (was `src/playback.c`): it pulls the part
stream, demuxes it to access units, and `Feed()`s them to LG's StarfishMediaAPIs while `libAcbAPI`
binds the decoded sink to the hardware video plane. The Starfish/ACB calls cross into C at the seam
`src/starfish.c` (outside this dir — edits there almost always pair with edits here); the **ABI and
bind-order gotchas for that seam live in THIS file**, below. The root `CLAUDE.md` carries only the
one-paragraph playback summary.

## Pipeline

In-process is the point: ACB can only bind an app-owned sink, which the earlier URI/out-of-process
path (`com.webos.media/load`, `start_playback()`) could not provide — that path is kept only as
dead-ish reference, and `docs/buffer-feed-plan.md` records the pivot (treat it as history, not
spec). The stream side: `PMS HTTP GET` → demux → per-lane access-unit
queues with byte-cap backpressure (`aq.rs`) → the pump `Feed()`s each AU to the Starfish pipeline.
The demuxer is **`ff.rs` — the libavformat the app BUNDLES (not the TV's), over a custom AVIO on
one of TWO transports** (design record: `docs/ffmpeg-demuxer-plan.md`; the hand-rolled `mkv.rs`
fallback is retired/deleted). **Which transport is decided once, in `ff::demux`, from the part
URL's scheme** — `AvioState` holds a source enum and `read_cb`/`seek_cb` dispatch: `http` →
`stream.rs`'s raw socket, `https` → `crate::curlio`. That matters *here* because teardown is a
different mechanism per arm: `engine::teardown` fires `stream::http_shutdown` at the socket **and**
`curlio::abort_active()` at the curl source's wake pipe, since a thread parked in `curl_multi_wait`
is not one any `shutdown(2)` of ours can reach. Exactly one of the two ever has anything to do. Ours ships beside the binary as `libav*-plx.so.*`, is `dlopen`'d by absolute path
and pinned to majors 63/63/61, and is built `--disable-network` with `file` as its only protocol —
so the AVIO is not merely how bytes reach it today, it is the only way they *can*. See the root
`CLAUDE.md` linking section for why. It
emits Annex-B video AUs (param sets prepended at each keyframe) and raw AC3/EAC3/AAC audio frames,
and seeks by time via `av_seek_frame` (libavformat's own Cues index).

## Threading model (this is the whole ballgame)

- `engine.rs` — the **main-thread-confined** session object. All ACB/Starfish *control* calls happen
  on the main thread; `engine` spawns the workers below.
- `pump.rs` — the **main-thread pump** (was `bufferfeed_pump`): each frame it drives bind → Play →
  feed and services seeks.
- `threads.rs` — the workers beside the demuxer: **`load_thread`** (construct Starfish + `Load()`,
  which owns its own GMainContext) and **`timeline_thread`** (the ~10 s `/:/timeline` progress
  reporter). The **demux thread body is `ff::demux`** (spawned by `engine::start_bufferfeed`): open
  the part URL, read+convert packets, push AUs to the two lanes; **and service seeks** — it
  `av_seek_frame`s on `seek_to_ns` between two `av_read_frame` calls, which is the whole seek
  mechanism (nothing interrupts it; see the seek gotcha below).
- `shared.rs` — the **only** cross-thread state (each field replaces a C `volatile` global — `g_*`).
  New cross-thread state goes here, behind the same discipline; don't smuggle it through a raw static.

**The main-thread rule is compiler-enforced.** `ffi.rs`'s `extern "C"` declarations are private to
that module, and every wrapper but one takes a `task::MainThread` — a `!Send` ZST `plex_run` mints
once and threads down. So does `engine::engine()` and the three other `ENGINE` accessors. Moving any
of it onto a thread stops compiling (the closure captures a `&MainThread`, which `task::spawn`
rejects). Two intentional holes, both worth knowing: `sf_load` takes **no** token because
`load_thread` runs it off-main by design, and `MainThread::assume()` is callable — so an `unsafe`
block inside a worker still defeats this. The rule for new code: take the token **iff** you reach
the seam or the Engine, so its presence in a signature keeps meaning something.

## Gotchas that bite (all verified in code)

- **C-from-C++ Starfish calls** go through `extern … __asm__("<mangled>")`. The object is an
  over-sized static buffer (`g_smp[65536]`) constructed in place by calling the ctor symbol —
  **never** hand it to C++ `new`/`delete` (real object size is unknown). Methods returning a
  `std::string` use a hidden sret first-arg; read the `char*` at offset 0 (SSO) for short replies
  like `"Ok"`/`"BufferFull"`.
- **Dolby Vision and Dolby Atmos have their own document: `docs/dolby-vision.md`.** The two
  payload nodes, the ACB audio forward, the Profile 5 one-tick fix and the instrument traps live
  there rather than here, because half of that record is about LG's binaries and the Dolby
  specifications rather than about our engine.
- **Starfish `Load` must be constructed with `uid = NULL`** (`SMP_ctor(g_smp, NULL)`), and in
  buffer-feed mode the app must **not** `LSRegister` its own `com.webos.media` client — either
  collides with the pipeline's uMS connection (CONN_FIND_ERR). See the comment in `load_thread`.
- **ACB bind order matters** (mirrors Kodi/ss4s): `setSinkType(MAIN)` → `setMediaId` →
  `setState(LOADED)` → *wait for decoded frames* → `setMediaVideoData(<sourceInfo envelope
  VERBATIM>)` → `setDisplayWindow` → `setState(PLAYING)`. The payload passed to
  `setMediaVideoData` is the **whole `sourceInfo` envelope** captured verbatim from the pipeline's
  callback (`sourceInfoRaw`), not a reconstructed one. Audio is owned by the pipeline — **never feed
  ACB an audio SINK or elementary stream**. That half is real and unchanged. Its long-stated
  consequence is not: `SOUND_ERROR_019` is a literal that exists in **no library on this
  television** (swept across ~70 harvested libraries including 92 MB of Chromium), and the clause
  has carried no evidence since the initial commit. **`AcbAPI_setMediaAudioData` IS used**, for a
  two-key METADATA descriptor — `{"audio":{"immersive":"ATMOS"},"context":"<mediaId>"}`, fired
  right after `acb_bind`, which is exactly where LG's own client fires it. Device-measured
  2026-08-21: `rv=1`, audio untouched (1600 AUs, `reply=O`, no error), and the set's own
  "Dolby Vision / Dolby Atmos" read-out captured on screen. Details and addresses:
  `acb_send_atmos` in `src/starfish.c`. `AcbAPI_setMediaVideoData`/`setState`/
  `setDisplayWindow` take a `long *taskId` out-param as their last arg — the 3-arg ABI is required
  (2-arg calls corrupt memory / segfault).
- **The Load payload's codecs must come from the `/decision` OUTPUT, not the source file.** A
  transcode changes the codec/rate; building the Starfish Load config from the *source* metadata gives
  the decoder the wrong description → **silent audio / glitches**. Read the output codecs from the
  transcode decision. For ADTS/HE-AAC, use the **CORE** sample rate from the AudioSpecificConfig (SBR
  doubles it). See `[[audio-payload-codecs]]`.
- **Subtitles are client-rendered here — the TV's HW subtitle engine is URI-mode only** and
  unreachable in buffer-feed. Both text (SRT/ASS) and image (PGS/VobSub) subs are decoded and drawn by
  us; don't expect the pipeline to burn or overlay them. See `[[tv-subtitle-engine]]` and the `plex/`
  soft-subs note. An image sub's rect coords are in **the subtitle stream's own authoring canvas** —
  1920×1080 for Blu-ray PGS but 720×480/576 for a DVD VobSub rip — so `ff::sub_canvas` reads that
  canvas off the decoder (via `avcodec_parameters_from_context`, no raw struct offset; the ABI proof
  is in its doc comment) and `player_hud::sub_screen_rect` scales the whole display set into the
  video rect. Assuming 1080p unconditionally is what made VobSub render as a corner postage stamp.
- **A seek NEVER interrupts the demuxer.** The pump publishes the target in `seek_to_ns` and the
  demux thread — the only thread that touches the `AVFormatContext` — `av_seek_frame`s on it
  between two reads. Do not reintroduce an interrupt: the pump used to `shutdown(2)` the socket to
  break the read so the outer loop would reopen and seek, and it could not work, because our AVIO
  is **seekable** (`seek_cb` reopens with a byte `Range`), so libavformat treats the broken read as
  recoverable, calls `seek_cb`, gets a fresh connection at the same offset and reads on.
  `av_read_frame` never returns an error, so the reopen never happens and the seek never lands —
  it just runs out the stuck-watchdog on pre-seek packets and escalates to a full reload. This
  survived a long time because the test suite's cases inherited a server-side `viewOffset`, so the
  seek under test usually had nowhere to go (fixed 2026-07-28; see the root `CLAUDE.md` testing note).
- **Seeks are in-place** (Kodi-style): flush + `av_seek_frame` to the target,
  then on the first post-seek keyframe `feed_stream` re-anchors the GStreamer segment
  (`setTimeToDecode` + `sendSegmentEvent`) — no reload/decoder re-init. A transcode seek instead
  restarts the encode at `&offset` with a full fresh `Load`. The rebase machinery
  (`pts_shift`/`rebase_pending` in `shared.rs`) keeps Starfish from ever seeing a PTS jump.
- **`frames` is SEEK-scoped, `seen_frame` is SESSION-scoped.** `pump` zeroes `SHARED.frames` as
  *part of applying* an in-place seek (it counts only post-seek frames, for the rebind + resume
  re-pause gate), so `frames == 0` does **not** mean "we have never shown a picture" — it is true
  for the whole of every seek. `SHARED.seen_frame` is the bit that answers that question: set beside
  `frames` in the presented callback, cleared **only** in `reset_session`. The HUD divides its two
  busy indicators on it (`ui::player_hud::busy_surface`); anything else asking "has this session put
  a picture on the panel" wants `player::seen_frame()`, not `frames() > 0`.
- **App-switch lifecycle** (handled in `app.rs`; details in the root `CLAUDE.md` gotchas): OS
  background suspends the buffer-feed preserving the session, foreground reloads and resumes with a
  single `Load`. Preserve the suspend/reload pairing if you touch playback.

## Verifying playback changes

**Start with `make check`** — the host unit suite (`cargo test --lib`, ~0.3s) covers a real
slice of this pipeline's pure logic: `ff.rs`'s `nal_end` bounds guard and AVCC→Annex-B conversion,
the AVIO abort guards (a seek after teardown must not open a second connection — graded on an accept
count), `stream.rs`'s socket lifecycle, `route.rs`'s direct-play-vs-transcode selection, and
`task.rs`'s `MainThread` token being genuinely `!Send`. Cheap enough that there is no reason to skip
it before a deploy.

But there is **no host *runtime*** — nothing above decodes a frame or touches Starfish/ACB (`ff.rs`
once gated `#[link]` directives out of `cfg(test)` to keep the pure logic host-testable; with
everything on `dynlib!` those are gone, so a test that actually calls FFmpeg now fails by taking
`dlopen`'s `None` branch on Darwin rather than by failing to link), and the host is Darwin while the TV is
Linux, which is why `tools/sockprobe.c` exists. So anything about *playback behaviour* is only
observable on device: deploy and read `/tmp/plxnative-events.log` (feed stats, bind steps,
seek/rebase, `RECEIVE_GOOD_VIDEO`). The `tests/` harness drives real playback per case — see the root
`CLAUDE.md` testing section (run as GUEST by default; never run two harness jobs at once).
