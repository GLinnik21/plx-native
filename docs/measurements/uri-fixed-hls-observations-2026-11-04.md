# URI HLS observations in the webOS 11.2.0 (o22) firmware image, 2026-11-04

This is a read-only analysis of two libraries from one archived LG firmware image in the DAS
firmware archive: `HE_DTV_W22O_AFABATPU` (OLED65C24LA, board o22 LX, firmware 43.00.74,
**webOS 11.2.0**; `epk/HE_DTV_W22O_AFABATPU/SOURCE.txt` records the download on 2026-10-27 and
`INDEX.md` the board and version). Everything below holds for that webOS 11.2.0 image only. It does
not establish the event format or adaptive controls of the webOS 4.5 dev set or of any other
firmware generation. The dev set's firmware, W19H (`HE_DTV_W19H_AFADABAA`, webOS 4.10.2), has no
`libpf-hls`: the only file with `hls` in its name under its `usr/lib` is
`usr/lib/gstreamer-1.0/libgsthls.so`, so which component plays HLS in URI mode there is not
answered by this note. The paths below are inside the image's unpacked tree
(`x/HE_DTV_W22O_AFABATPU/rootfs.pak.unsquashfs/`); the harvested copies used for the decompile
are in `lab/uri-HE_DTV_W22O_AFABATPU/bin/`. No library was pulled from or run on the TV for this
analysis.

| Binary | Path in the unpacked image | SHA-256 | ELF build ID |
| --- | --- | --- | --- |
| `libpf-hls-1.0.so.1.0.0` | `usr/palm/resources/com.webos.resource.media-framework.resource/lib/` | `3a444ed2099851085aaed7e5939b2eb23dd098559ca5bf79ef54e1cb85742a43` | `cef0019a037a0234712df48adac2b267b34bd667` |
| `libpf-1.0.so.1.0.0` | `usr/lib/` | `059265268fac05f49fdc7bea1bf1d9ae892d9683709eba0bf19905517c152bd6` | `5e9682a54b8fbe534a6cb1cbf739bc710af3c2ed` |

The hashes of the unpacked files and of the lab copies are identical, and the build IDs were read
from the lab copies.

## What the producer does

Decompilation of `mediapipeline::HlsPipeline::updateBufferingInfo()` at ELF virtual address
`0x1c2ac` in `libpf-hls-1.0.so.1.0.0` shows that it reads GStreamer's
`current-level-time-video` and `current-level-time-audio` values, chooses the applicable buffered
range, and combines it with `mediapipeline::Pipeline::position()`. Before formatting the range,
it divides the 64-bit time values by the literal `1,000,000,000` at ELF file offset `0x1c960`.
The formatter has the fields `beginTime`, `endTime`, `remainingTime` and `percent`. This producer
therefore reports **integer seconds**, with subsecond precision lost. The selected A/V minimum
is a pipeline buffer fact; it is not a measurement of network transfer rate or a proof of a
presented picture.

`HlsPipeline::updateStreamingInfo()` at `0x1bfc4` sends `currentBitrate` and `contentBitrate`
through its callback, and separately sends the number of rates and current rate index. Those
are stream/rendition metadata. Neither function measures a completed transfer's byte count or
active fetch duration. `mediapipeline::Pipeline::notifyBufferRangeInfo(long long, char const*)`
at `0x71cc0` in the same image's `libpf-1.0.so.1.0.0` forwards its text to installed callbacks; it does not
attach or validate a media ID there. A higher service layer may supply an identity envelope,
but these functions do not prove that it does.

The [webOS 4.5 URI experiment](uri-acb-dass-2026-11-02.md) logged `bufferRange` beginning and
ending near 0–58 as `currentTime` advanced in milliseconds. That is consistent with seconds,
but the webOS 11.2.0 producer does not prove the 4.5 producer's units. The experiment logged only the
selected `streamingInfo` event name, not a transfer rate or rendition identity.

## Application boundary and missing evidence

The current [`uri_protocol.rs`](../../rust-modules/play-uri/src/uri_protocol.rs) recognizes
`bufferRange` and `streamingInfo` but retains them only as bounded diagnostics. It validates
nested media IDs for `currentTime`, `seekDone` and several lifecycle events, but not for
`bufferRange` or `streamingInfo`. Before either can drive a playback decision, the URI session
must verify its current subscription/attempt and any event identity, parse bounded numeric
facts, establish the units for the actual target firmware, and discard late callbacks after
release. `streamingInfo` bitrate must remain declared/selected bitrate, never measured capacity.

The Feed HLS controller takes completed per-segment bytes, active fetch microseconds, media
duration, and separate audio/video playable tails (`abr::SegmentSample` and
`playback_contract::adaptive::ActiveQuantum`). URI callbacks above supply none of the completed
acquisition facts. Feeding a `bufferRange` interval or `currentBitrate` into that controller as a
segment sample would invent evidence. A separately timed, bounded HTTP transfer from the same
authorized source could provide a `CapacityObservation` to the existing pure capacity estimator
for bootstrap, but it would not certify a live Feed-style upshift or a URI variant change.

There is no evidenced URI rendition-switch command in the current URI driver. A fixed-variant
app controller would need one common owner to choose a certified fixed source, then use addressed
Stop, release proof and Start, and confirm the new stream before committing quality. The
application must keep the old source and encoder resource until that outcome is known. Native
master HLS has a platform controller; the current `AppFixedControllerUnavailable` decline for
URI fixed playlists describes this integration gap, not a claim that the firmware refuses them.

## Reproduction

Use the repository's `.agents/skills/decompile-tv-lib/decomp.sh` with the DAS lab directory for
this image (`lab/uri-HE_DTV_W22O_AFABATPU`, which holds `bin/`, the Ghidra project and the
scripts); do not run its `pull` command for this note. The script rebases the two function
addresses by `0x10000` in Ghidra. For example, with `DAS_LAB` set to that directory:

```sh
DECOMP_LAB="$DAS_LAB" .agents/skills/decompile-tv-lib/decomp.sh fn libpf-hls-1.0.so.1.0.0 updateBufferingInfo 1
DECOMP_LAB="$DAS_LAB" .agents/skills/decompile-tv-lib/decomp.sh fn libpf-hls-1.0.so.1.0.0 updateStreamingInfo 1
DECOMP_LAB="$DAS_LAB" .agents/skills/decompile-tv-lib/decomp.sh fn libpf-1.0.so.1.0.0 notifyBufferRangeInfo 3
```

Check the SHA-256 values above before using the inventory. The addresses, divisor, callback
flow and bounded string field names are the evidence; no decompiled vendor body is copied here.
