# player/ — the buffer-feed video engine

This is the in-process **buffer-feed** playback engine (was `src/playback.c`): it pulls the part
stream, demuxes it to access units, and `Feed()`s them to LG's StarfishMediaAPIs while `libAcbAPI`
binds the decoded sink to the hardware video plane. The **ACB bind order, the `sourceInfo`-verbatim
rule, the audio-owned-by-pipeline rule, and the 3-arg taskId ABI** all live in the C seam
(`src/starfish.c`) and are documented in the **root `CLAUDE.md` gotchas** — read those before
touching the bind/feed control calls.

## Threading model (this is the whole ballgame)

- `engine.rs` — the **main-thread-confined** session object. All ACB/Starfish *control* calls happen
  on the main thread; `engine` spawns the workers below.
- `pump.rs` — the **main-thread pump** (was `bufferfeed_pump`): each frame it drives bind → Play →
  feed and services seeks.
- `threads.rs` — the three workers: **`stream_thread`** (demux: open the part URL, run the MKV
  demuxer, push AUs to the queue; loops on seek by re-opening at a byte `Range:` and resyncing to the
  next Cluster), **`cues_thread`** (cue-preflight: parse the MKV header, fetch the Cues by Range, build
  a time→byte index; CBR estimate until ready), **`load_thread`** (construct Starfish + `Load()`, which
  owns its own GMainContext).
- `shared.rs` — the **only** cross-thread state (each field replaces a C `volatile` global — `g_*`).
  New cross-thread state goes here, behind the same discipline; don't smuggle it through a raw static.

## Gotchas that bite (all verified in code)

- **The Load payload's codecs must come from the `/decision` OUTPUT, not the source file.** A
  transcode changes the codec/rate; building the Starfish Load config from the *source* metadata gives
  the decoder the wrong description → **silent audio / glitches**. Read the output codecs from the
  transcode decision. For ADTS/HE-AAC, use the **CORE** sample rate from the AudioSpecificConfig (SBR
  doubles it). See `[[audio-payload-codecs]]`.
- **Subtitles are client-rendered here — the TV's HW subtitle engine is URI-mode only** and
  unreachable in buffer-feed. Both text (SRT/ASS) and image (PGS/VobSub) subs are decoded and drawn by
  us (canvas authored at 1920×1080); don't expect the pipeline to burn or overlay them. See
  `[[tv-subtitle-engine]]` and the `plex/` soft-subs note.
- **Seeks rebase the fed PTS timeline** on the first post-seek keyframe (`pts_shift`/`rebase_pending`
  in `shared.rs`) so Starfish never sees a jump. Clusters start on a keyframe; the resync lands there.
- **Never feed audio to ACB** (SOUND_ERROR_019) — the pipeline owns audio; ACB binds video only. (Root
  gotchas, restated because it's easy to forget when adding a feed path.)

## Verifying playback changes

There's no host runtime — deploy and read `/tmp/poc-events.log` (feed stats, bind steps, seek/rebase,
`RECEIVE_GOOD_VIDEO`). The `tests/` harness drives real playback per case — see the root `CLAUDE.md`
testing section (run as GUEST by default; never run two harness jobs at once).
