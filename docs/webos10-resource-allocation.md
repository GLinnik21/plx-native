# webOS 10 refuses every H.264 Load — and so refuses every server transcode

**Status: MEASURED, device-verified both ways, and NOT FIXED on any branch.** The one-line change
below was proven in the lab slot that found the bug and then deliberately reverted, because it is
not the ABR defect that session was booked for. This file exists so that decision stays a
*decision* rather than becoming a thing nobody wrote down.

> **Provenance.** LG Cloud Test Lab, 2026-08-27, one hour, on a set nobody here owns:
> board `k24` / `K24_DVB`, **release 10.3.1**, booked as "webOS24". The log came back through the
> Lab Diagnostics bridge (`docs/lab-diagnostics.md`) — there is no ssh on those sets, so this is
> the only class of evidence obtainable from them at all.
>
> **No raw snapshot from that session may be pasted into this repository.** The same hour found
> that `stream: GET /identity DNS FAILED host=…` carries a hostname past all three redaction
> layers. Every identifier below is a placeholder per `docs/shared-servers.md`.

## The defect

`player/engine.rs`'s streamed A/V path declares the Starfish sink envelope as the panel maximum,
**regardless of the codec being fed**:

```rust
// Sink envelope = the panel max (4K) regardless of codec; the pipeline reads the
// true dims from the bitstream (SPS), so this is just a ceiling and is correct for a
// 4K stream (HEVC transcode / HEVC direct-play) AND harmless for a 1080p H264 file.
let (mw, mh) = (3840, 2160);
```

with `build_av_payload` additionally forcing `"maxFrameRate":60`. So every streamed Load carries
`adaptiveStreaming: { maxWidth: 3840, maxHeight: 2160, maxFrameRate: 60 }`.

**The final clause of that comment is true on webOS 4.10 and false on 10.3.1.** The pipeline
allocates against the DECLARED ceiling rather than against the bitstream, and no AVC decoder on
that SoC can allocate 4K60. The Load is refused:

```
smp_cb type=18 num=601 str=Resource Allocation Error
```

**The control makes it airtight.** The identical envelope with `"H265"` in the codec node loaded
and played 197+ frames in the same session, minutes apart. The difference is the declared codec,
not the link, the server, the content or the hour.

## Why this is a ship-blocker rather than an edge case

Every Plex server transcode targets H.264 (or falls back to it). So on webOS 10:

- **HEVC direct play** — fine.
- **Every transcode** — refused at Load, before a frame, before the video plane binds.
- **ABR never runs at all** on that content: the rung machinery lives downstream of a Load that
  never completes.

The dev set is webOS 4.10, where the over-declaration is harmless, which is exactly why three
years of testing never showed it.

## The A/B, which was run

Same slot, same set, byte-identical declaration `load: v=H264 a="AAC" fps=0.000`:

| build | result |
|---|---|
| unpatched | `601 Resource Allocation Error`, **twice** |
| patched | `SMP loadCompleted` in **117 ms**, then primed/Play, HLS segments, and ABR commits (`committed Down to 2000kbps 1280x720` -> `720kbps 854x480`) |

The patch was `let (mw, mh) = if vc == "H265" { (3840, 2160) } else { (1920, 1080) };`.

## Why that patch is a DEMONSTRATION and not the fix

`1920x1080` under-declares a genuine 4K H.264 file, which was never tested in that slot. Two
further findings constrain the real fix:

1. **"Take the ceiling from devcaps" is refuted.** That was the first answer and the session kills
   it: the set's own `device_codec_capability_config.json` claims **4096x2176 for H.264 too**
   (`devcaps: hevc=true 4096x2176` is the MIN across both decoders' rows). Width and height were
   never the binding constraint, so a devcaps-derived raster would have declared 4K again and
   failed again.
2. **The discriminator is almost certainly `maxFrameRate`** — which `devcaps.rs` parses and then
   **drops by design** (`CLAUDE.md` already records the profile bounding no frame rate). Declaring
   4K **at 60** is what cannot be allocated. That also fits (1): the raster was legal, the rate
   was not.

So the principled fix declares the dimensions **and rate the stream will actually carry**, which
the demux knows, rather than a panel ceiling — and it needs a 4K H.264 leg to confirm, which no
session has run.

## What else that hour settled (all firsts, all on 10.3.1)

- The webOS 5+ **`VP_EXPORTED`** path works: `_Window_Id_N` created, spliced into the payload,
  `placed rv=1`, re-placed once real dimensions arrive.
- The **bundled FFmpeg** 63/63/61 loads on a set shipping FFmpeg 6 itself.
- The **pinned-TLS** diagnostics upload survives libcurl 7.82 / OpenSSL 3.0.13.
- **4K HEVC DV Profile 8 + Atmos** direct-plays from a shared server over the public internet.
- The lab's virtual remote **does** send colour keys; **BLUE = wcode 489**, same as the Magic
  Remote (484/485 exist there and map to nothing of ours).

## Second defect from the same hour, also unfiled

A **remote cold start burns ~16 s** probing LAN addresses that cannot work from abroad, before
falling back to the public HTTPS URI. Not measured further; recorded so it is not rediscovered.

## What would settle the remaining question

One lab slot, a 4K H.264 item, and three legs: panel ceiling (expect 601), stream-derived
dimensions at the stream's own frame rate (expect load), and stream-derived dimensions at 60
(expect 601 if the rate is the discriminator, load if it is not). That third leg is the experiment
— the other two only bracket it.
