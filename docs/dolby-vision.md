# Dolby Vision and Dolby Atmos on webOS 4.10.2

The record of how this app came to declare Dolby Vision and Dolby Atmos to LG's pipeline, what the
television's own binaries say about it, and what the Dolby specifications do and do not require.
Written 2026-08-21, the day both landed; every number in it was measured on the dev set (LG
49SM9000PLA, webOS 4.10.2) or read out of that set's libraries unless it carries a page citation.

**Read this before touching `Dovi::presentation`, `with_dolby_hdr_info`, `with_immersive`,
`acb_send_atmos` or `pts_nudge_ns`.** Several of the conclusions here replaced earlier ones that
were confidently wrong, and the replaced versions are kept alongside because the way they failed is
the useful part.

---

## 1. What ships

| Profile | Base layer | On the dev server | What we do |
|---|---|---|---|
| **5** (`dvhe.05`) | none — IPT-PQ, CCID 0 | 1 | **declare + direct play** |
| **8** (`dvhe.08`) | HDR10 (CCID 1) | 32 | **declare + direct play** |
| **7** (`dvhe.07`) | dual-layer, CCID 6 | 1 | **refuse → transcode** |

Census taken 2026-08-21 by fetching all 540 leaves and reading every `streamType: 1` stream: 551
video streams, 34 of them Dolby Vision, in exactly those three shapes. 33 of the 34 direct-play
with a declaration.

**Profile 7 is the one real gap and it is structural, not policy.** Its picture is split across a
base and an enhancement layer — two elementary streams — and BUFFERSTREAM buffer-feed hands the
pipeline **one**. There is no payload key for the second; `Dovi::presentation` refuses on
`el_present` before it reaches the declaration arm, which is also what keeps the emitted node's
`trackType` at `"single"` and makes the pipeline's `dv-dual-svp` secure-video-path flag — which
this app cannot satisfy — unreachable. Whether `gst_dvbin_pipeline_dovi_dual` (which sits beside
the `_single` we do use) can be fed from a single appsrc is unexamined.

Profiles not present here: **4** is dual-layer and deprecated (P&L §1.1 p. 6); **9** is single-layer
**AVC**, and would pass our gate but `with_dolby_hdr_info`'s `video != "H265"` guard would silently
drop the node — harmless, since CCID 2 means the base layer is SDR-compatible, but it is a gate/
payload disagreement the tests do not cover; **10** and **20** are AV1 and MV-HEVC, and this panel
has no decoder for either.

---

## 2. The Load payload: two nodes, both recovered from the set's own binaries

Both live in `option.externalStreamingInfo.contents`, spliced at the `"provider":"plxnative"`
anchor — the last key of `contents`, asserted present exactly once by a test so a `replace` cannot
double-splice.

### 2a. `DolbyHdrInfo` — the video half

```json
"DolbyHdrInfo": { "trackType": "single", "encryptionType": "clear", "profileId": 5 }
```

`libpf-1.0.so.1.0.0`'s `CustomPipeline::parseOptionStringSpi` builds the literal key
`option.externalStreamingInfo.contents.DolbyHdrInfo`, asks `Options::checkKeyExistance` for it, and
on a hit sets `hasDolbyHdrInfo` — **unconditionally, before a single sub-field is read and with no
platform gate**. `getVideoCaps` then appends `dolby-vision=TRUE` (plus `dolby-vision-profile` when
`profileId != -1`) to the `video/x-h265` caps it was already building. Without the node, appsrc
gets plain `video/x-h265` and nothing downstream can engage Dolby Vision at all.

Three things that look like they should change and do not:

- **the codec string stays `"H265"`.** `getVideoCaps` maps H265 to `video/x-h265` and that branch
  falls THROUGH into the Dolby Vision tail. There is no DVHE/DVH1 row in its codec table — those
  literals belong to AdaptivePipeline's RFC-6381 parser, a different pipeline. LG's own Chromium
  client also reports `codec.video = "H265"` for a Dolby Vision stream.
- **`profileId` must be a JSON integer** (`getInt`). Quoting it leaves the pipeline's `-1` sentinel,
  which still yields `dolby-vision=TRUE` with only the profile hint missing — a legitimate fallback.
- **nothing declares platform support.** `libplayerAPIs::generateJsonPayloadForPlayer` injects
  `platformSupportDolbyVision` / `supportDolbyTVATMOS` itself from its configd cache, at the tree
  ROOT as siblings of `option`. Sending our own would be a second opinion on a question the library
  answers for itself.

### 2b. `contents.immersive` — the audio half

```json
"immersive": "ATMOS"
```

Neither half of this is guessable and a wrong one is silent, so both were read out:

- `libpf` holds the literal key path `option.externalStreamingInfo.contents.immersive`, logs it as
  `PF_EXT_IMMERSIVE : %s`, and carries the string onto the audio caps it builds —
  `audio mediaInfo … channels[%d] language[%s] … immersive[%s] role[%s]`. It is `%s` the whole way:
  **libpf validates nothing**, so the value cannot be inferred from libpf.
- The value therefore has to come from whoever fills it, and **the bare literal `ATMOS` exists in
  exactly ONE library on this television: `libcbe.so`** — Chromium's media backend, the path LG's
  own web apps (Plex's included) take. In its string pool that literal sits immediately after
  `immersive` and in the same run as `externalStreamingInfo`, `esInfo`, `seperatedPTS`, `provider`,
  `DolbyHdrInfo`, `encryptionType`, `profileId` and `contents` — our payload, key for key.

**Whether a track HAS Atmos is `Stream.profile` and only there** — `"dolby digital plus + dolby
atmos"`, while its `audioChannelLayout` is a plain `"5.1(side)"` and its `title` is null. A client
reading the layout, the title or the channel count badges nothing, forever and silently. Dolby's
own AC-4 spec §3.1.1.1 says the same from the other end: *"It is not possible to derive whether
content is branded as Dolby Atmos by inspecting the channel configuration."*

It is taken off **the track we actually picked**, not off the part — a film ships an Atmos track
beside a plain one and a commentary — and only on the direct-play branch, because a transcode
re-encodes the audio and its Atmos is gone.

---

## 3. Forwarding Atmos to ACB, and a rule that was never true

Sending the node is not enough for the television's on-screen read-out. `libpf` puts `ATMOS` on the
audio caps and the pipeline then hands **us** a callback — `smp_cb type=7`, carrying
`{"track":-1,"dualMono":false,"immersive":"ATMOS"}` — which LG's own client turns into an ACB call.
We ignored it until 2026-08-21.

`libcbe.so`'s `media::MediaAPIsWrapper::SetDolbyAtmosInfoToACB` @0x01b976f0 — the only caller of the
ACB audio entry point in ~70 harvested libraries — builds a two-key jsoncpp object and hands it to
`Acb::setMediaAudioData` on the **same handle as the video bind**, with a NULL taskId:

```
{"audio":{"immersive":"ATMOS"},"context":"<mediaId>"}\n
```

`context` is the identical string `acb_bind` already passes to `setMediaId`. Carrying it is the
entire reason LG synthesises this object instead of forwarding the callback string:
`StarfishMediaAPIs::handleAudioInfoEvent` builds that string from `track` and `dualMono` only and has
**no context**, while `generateVideoInfoPtree` @0x3755c puts one in the VIDEO envelope we already
forward verbatim and which already works. The same object, one codec over.

It fires immediately after `setMediaId` (libcbe 0x1b98d78, on LOADCOMPLETED) — before the
frame-gated `setMediaVideoData`. No decoded frame is needed and no state is read.

### The `SOUND_ERROR_019` retraction

`src/starfish.c` and `rust-modules/src/player/CLAUDE.md` said, from the **initial commit**
(`f2523483`), that audio must never be fed to ACB because it causes `SOUND_ERROR_019`, and that
`setMediaAudioData` is therefore unused. The clause sat beside the 3-arg-taskId note, which does
carry its evidence.

**That literal exists in no library on this television.** Swept three ways across the whole harvest
— `strings` per file, a `SOUND_ERROR|SOUND_*ERR` pattern, and a raw byte search — including 92 MB of
Chromium. The only `SOUND_` hits anywhere are `SOUND_INITIALIZED` in a udev table and
`WIRELESSSOUND_SMSC` in `libdile_i2c`. No log line containing it has ever been committed.

What the rule is **right** about is the audio elementary stream: the pipeline owns it and we still
never hand ACB a sink. `AcbAPI_setMediaAudioData` @0x1836c is instruction-for-instruction the same
316-byte shape as `AcbAPI_setMediaVideoData` @0x16fac, and below it `ACB::AcbCore::setMediaAudioData`
@0xfda4 parses the JSON, dedups against its cached copy, and posts one async
`luna://com.webos.service.acb/setAudioInfo` with `{"appId":…,"pipelineId":<mediaId>,"audioInfo":…}`.
A validity check, a dedup and one Luna post. It cannot reach a sink.

Armed behind a trigger first, measured, then defaulted: `rv=1`, 1600 audio AUs fed with `reply=O`
and no error, playback unbroken, and the set's own "Dolby Vision / Dolby Atmos" read-out
photographed on screen.

**Portability:** `setMediaAudioData` is deliberately NOT in `vp_mode`'s all-present AND-gate. It is
exported on 3.9.2 / 4.4.2 / 4.10.0 but **absent on 2.2.3 and 3.4.0** (`tools/fwcompat.py --lib
libAcbAPI.so.1.0.0 --grep setMedia`); gating ACB on it would refuse ACB outright there and take all
video with it. An Atmos read-out is not worth a black screen.

---

## 4. The Profile 5 stutter: one 90 kHz tick

A declared Profile 5 pulsed at ~2 Hz. It took a day, ten agents and six refuted hypotheses; the
answer is one increment.

### The mechanism

`gstdualsequencer.c:606` (`libgstdualsequencer.so` 0x25b0–0x25e4, DWARF intact) keys the LUT entry
it hands the display firmware with a **double truncation** of the DM packet's nanosecond PTS:

```
ulTimeStamp = trunc(trunc(pts_ns) * 9 / 100000)        // ns -> 90 kHz ticks
```

`DOVI_SWSync_SetDoviLUTnMap` (`libkadaptor.so.2.0.1` 0xe30e8) then scans all 95 slots for **exact
32-bit equality** on that key — no tolerance, no nearest match. With LG's own level-2 KADP logging
armed mid-playback, **38 of 40** misses showed the key written into the ring to be exactly the key
the firmware asked for **plus one**. One tick. 11.1 µs. On a lookup with zero slack.

A miss costs no frame — the firmware reuses the previous entry — so the symptom is a **stale tone
mapping for 2 frames in every 12**, i.e. a 0.5005 s pulse in brightness, *not* judder. That
distinction cost most of a day on its own; see §5.

### The fix

Feed the pipeline a PTS one nanosecond **lower** (`pts_nudge_ns`, default `-1`, overridable with
`/tmp/plxnative-ptsnudge=<ns>`; `0` disables). Alternating unseeked legs, same title, same binary,
scene controlled by alternation:

```
nudge = -1   misses 1        nudge = 0   misses 81
nudge = -1   misses 1        nudge = 0   misses 81
```

and on the shipped default, 90 s of playback with nothing armed: **3 misses**, against 160 in 45 s
before.

**Why the doc comment does not model this to the nanosecond, and should not pretend to:** our fed
PTS is not passed through. The pipeline re-timestamps by NEAREST-rounding on the 1001/24000 lattice
(measured: the DM branch's PTS alternates −0.333/+0.333 ns against the exact rational), so the path
from one nanosecond on our side to one tick on theirs runs through code we do not have. What is
claimed is what was measured.

### Six hypotheses that were wrong, and how each died

Kept because every one of them was believed, and because the pattern of failure is the lesson.

| Hypothesis | Killed by |
|---|---|
| **"the metadata write never happens"** — the premise the whole first day rested on | The log mask. See §5. |
| **"the MD entry was never written for the missed frames"** | Traces already in hand: `got pair` / `extra_meta2` 608 / 632 / 658 / `push_dual OK` are all **1233** in a 1233-frame run, with zero bail lines. Every frame wrote. Zero device cost. |
| **"the ring slot was recycled"** (95-slot ring) | Measurement: the producer leads the display by **4 frames** (p50 0.167 s, max 0.250 s). ~20× inside the ring. |
| **"insufficient pipeline lead"** — the firmware asks further ahead than we produce | Same measurement, plus arithmetic: no constant lookahead yields 16.7 % with zero variance. |
| **"+1 ns fixes it"** — right mechanism, wrong sign | Measured inert: 163/165 against 164 for zero. The earlier A/B that seemed to show +1 making it *worse* (118 vs 230) ran under `autoseek`, where `pts_shift` re-phases the nanosecond lattice; that pair was variance, and reading it as a treatment effect was an error of ours, not the instrument's. |
| **"frames are being dropped"** | Nothing is dropped anywhere (push_dual 1233 / read_picture 1233 / callback 1234) and the sink is metronomic: gap p50 41.71 ms, p95 41.89, p99 43.17, **zero gaps over 120 ms in 52 s**. Upstream jitter of up to 133 ms exists and is fully absorbed. |

---

## 5. Instruments, and three that were silent by construction

**This is the section to read first if an investigation here stalls.** Every wrong turn above
descends from an instrument that could not see its own subject.

1. **LG's KADP log masks gate BITWISE, not by threshold.** `KADP_LOGM_WriteLog` (libkadaptor
   @0x88c34) tests `(1 << level) & rec[0x20] & ~rec[0x24]`, and `kad-hdr` ships with
   `enable=0x0000000b` — levels 0, 1 and 3, **bit 2 clear** (read off the live device, not inferred).
   `DOVI_MDAsync_WriteOTTMetaData`'s only unconditional line is level 2, so **a perfectly healthy
   metadata writer logs nothing** and "it never appears in the log" is a fact about the mask.
   **`tools/logmprobe`** (`make logmprobe`) reads and flips those masks: the table is mmap'd
   `MAP_SHARED` from `/dev/lg/logm`, so a second ssh session can arm a level **on a running app** —
   no rebuild, no relaunch, no perturbation of the session being measured. Read-only unless given
   `set`/`clear`. It ended this investigation in one run.
2. **`vtick=` / `vgap=` in the heartbeat are a 5 Hz position probe, not a frame counter.** They
   count `smp_cb type=0`, which the pipeline emits five times a second; at 23.976 fps they cannot
   resolve a two-frame loss, and they read a flat `vgap=201ms` straight through a visible stutter.
   The comment above them said "a frame was PRESENTED" — written from the callback's NAME, never
   from its rate.
3. **`GST_DEBUG=dualsequencer:6` does NOT perturb.** The reputation is real but belongs to level
   **9**. Calibrated by measuring the same scene both ways: **123 LUT misses uninstrumented, 122
   traced**. It is the only per-frame cadence instrument this project has, and it was avoided for
   months. Armed via `/tmp/plxnative-gstlog`, written to `plxnative-gst.log` in the runtime dir.

Also useful: `/tmp/plxnative-dvnonode` drops the payload node while keeping direct play (the fine
bisect — the picture is then deliberately WRONG, for judging cadence not colour); `/tmp/plxnative-nodv`
withholds the declaration entirely, which re-imposes the old refusal and therefore bisects "declared
vs transcoded"; `/tmp/plxnative-nofps` withholds the esInfo fps rational (withholding it is
**worse** — 163/160 misses against 82/3 — which is how we learned the pipeline's lattice depends on
what we send, shortly before learning it does not depend on it in the way we hoped).

---

## 6. What the Dolby specifications actually say

Read against six Dolby PDFs (ISOBMFF v2.8, Profiles & Levels v1.5, HLS v3.0, DASH v3.0, AC-4 v1.0,
Immersive Audio Channels) plus Rec. ITU-T H.265 v8. **Framing first:** none of the four Dolby
documents contains a conformance-language clause and **the word "shall" appears zero times in any of
them**; they use lowercase "must" as house style, and P&L's notices page says the document *"is
provided solely for informational purposes"*. The actual conformance instrument is named in P&L §3
p. 18: *"Every Dolby Vision playback device must pass Dolby Vision system development kit
certification."* So "the Dolby spec SHALL-requires X" is a sentence nobody can write from this set.

### 6a. There is no player contract

**No specification in the set places any requirement on any component to deliver, preserve, forward
or not discard the RPU.** ISOBMFF has zero occurrences of "discard", "preserve", "parser", "demux";
P&L's only profile-scoped device MUSTs all land on profile 20; HLS's are scoped to Apple devices and
are producer-side; DASH uses the word "player" once in 22 pages. A `h265parse`-style stage between a
Feed() seam and a display engine is an actor with no representation in the corpus. So "does LG's
pipeline dropping NAL 62 count as a defect" is not settleable from documents — only against the
firmware.

What the set does give, all sub-normative and all pointing one way: Dolby's exemplary player logic
(ISOBMFF Annex B p. 33) offers profile 5 exactly two outcomes — *handle as Dolby Vision* or *reject
playback* — with no "decode as the base layer signals" branch, that branch existing only for
cross-compatible sample entries; the one sentence permitting removal of DV elements (P&L §1 p. 4) is
explicitly scoped to profiles with a cross-compatible base layer, which profile 5 is not; and the
RPU is normatively required to stay **unencrypted** (ISOBMFF §5 p. 27), a rule only coherent if
something downstream parses it in-band.

### 6b. "The RPU is HEVC NAL type 62" is our measurement, not a Dolby fact

**No NAL unit type number appears anywhere in any of the four Dolby documents.** What Dolby says is a
table entry — P&L §2.1 Table 1 p. 9, profile 5's *"Metadata carriage mechanism: Unspecified NALu"* —
and a pointer out to ISO/IEC 23008-2 §7.4.2.2 plus MP4RA registration. The number lives in the
licensee-only *Dolby Vision Consumer Decoder Specification*. Contrast AV1, where Dolby pins the
carrier to the byte (`metadata_specific_parameters = 0xB5003B`); for HEVC it declines.

Our 97/97 scan of the test asset is good evidence about **that file**. Say "measured", every time.

### 6c. A conformant HEVC parser is *not* entitled to discard UNSPEC48..63

The premise that a strict parser may drop them is wrong on the law, and the correction changed the
investigation's direction. H.265 §7.4.2.2 p. 65 carries two deliberately different paragraphs: units
in `UNSPEC48..UNSPEC63` *"shall not affect the decoding process specified in this Specification"*;
decoders *"shall ignore (remove from the bitstream and discard) the contents of all NAL units that
use **reserved** values"*. The only removal mandate is scoped to **reserved** (41..47). §3.139 vs
§3.187 keep the classes apart — reserved means *ITU may claim it later*, unspecified means *this
document permanently declines it*. NOTE 1 delegates the unspecified range *"as determined by the
application … defined or managed in the controlling application or transport specification"*, which
here is Dolby's.

Three more H.265 statements cut against blind discard, and one vindicates our asset: §3.5 binds a
`UNSPEC56..63` unit to *the preceding VCL NAL unit in decoding order*; §7.4.2.4.4 pp. 70–71 says
such units *shall not precede the first VCL NAL unit of the access unit* — our measured layout
(AUD 35, SEI 39, slice, 62) is exactly conformant; and clause 10, the only normative removal process
in H.265, removes by `TemporalId` and `nuh_layer_id` and never touches 48..63. The strongest true
statement left is **"H.265 does not forbid dropping them"** — permitted by silence.

### 6d. Confirmed constants worth having

- **`dvcC` for profiles ≤ 7, `dvvC` for 8–10** (ISOBMFF §3.2 p. 13), exactly one, mandatory. Profile
  5 uses `dvcC`. `dv_version_major` must be 1, `rpu_present_flag` and `bl_present_flag` must be 1,
  and **`dv_md_compression` must be 0 for profiles ≤ 7** — a cheap conformance check on any asset.
- **`dv_bl_signal_compatibility_id`, complete list** (P&L §2.1 pp. 10–11): 0 none/IPT-PQ · 1 HDR10 ·
  2 SDR · 3 reserved-proprietary · 4 HLG · 5 reserved · **6 Ultra HD Blu-ray Disc HDR** · 7 reserved ·
  15 reserved-proprietary. **Our server reporting CCID 6 for the Profile 7 file is exactly correct** —
  it is the only value Table 1 assigns to profile 7. It looked surprising only because **ISOBMFF v2.8
  never mentions CCID 6 at all** and its Annex B pseudocode accepts only 0/1/4. P&L is the authority;
  ISOBMFF defers to it.
- **Profile 7 is the only current dual-layer profile.** `BL:EL = N/A` in Table 1 means "no
  enhancement layer" for 5, 8, 9, 10 and 20. Withdrawn: 0, 1, 2, 3, 4, 6, and profile 8 with CCID 5.
- **Profile 5's colour is standardised, not undefined.** CCID 0 is BT.2020-2 primaries, **"PQ with
  reshaping"**, IPT-PQ-C2 matrix per SMPTE ST 2128:2023, 4:2:0 — and JVET standardised the code point
  (15). VUI is *optional* for profile 5 and is either `1,2,2,2,0` ("original") or `1,9,16,15,0`
  ("preferred"). Footnote [b] p. 11 is a trap: even signalled as transfer 16, the actual
  characteristic is "PQ with reshaping", so a stage applying textbook PQ produces a colour error.
- **Profile 5 may legally be `dvhe`, not only `dvh1`.** The pair differs in parameter-set carriage
  (`dvh1`/`hvc1` = sample entries only; `dvhe`/`hev1` = in-band permitted). Tooling that assumes
  profile 5 is always `dvh1` is wrong. Codec string is
  `[fourCC].[profileID].[levelID]`, e.g. `dvhe.05.07`.
- **Dolby's own architecture puts the DV stage AFTER the video decoder** — P&L §2.2.1 p. 17 explains
  that DV level IDs are unrelated to HEVC levels because of *"a postcompression Dolby Vision
  processing pipeline and an MPEG reference video elementary stream decoder"*. That is the model our
  `dvbin → dvsplitter → {lxvideodec, dvmdparse} → dualsequencer` chain implements.
- **One RPU per frame is a glossary definition** (P&L p. 25), not a normative clause. Our 97/97 is
  consistent with it; neither proves the other.

### 6e. Atmos does not require AC-4

If anyone is carrying the assumption that the ATMOS badge needs AC-4, it is refuted: HLS §3.3.1 p. 14
declares Atmos as `CODECS="dvh1.05.07,ec-3"` + `CHANNELS="16/JOC"` in every example — 42 occurrences
of "atmos", **zero of AC-4 in the whole document** — and AC-4's own glossary p. 34 defines Dolby
Atmos as *"delivered via multiple audio codecs including Dolby Digital Plus, Dolby AC-4, Dolby MAT,
and Dolby TrueHD."* Our path is E-AC3 with JOC, which is what the dev server's asset carries and what
the television decodes (`_AUDIO_DecodedInfoCb_AC3: eac3 : 1, ATMOS : 1, channelNum : 6`).

---

## 7. Open

- **Profile 7.** One film on the dev server. Would need to know whether `gst_dvbin_pipeline_dovi_dual`
  can be fed from a single appsrc, or whether a second one can be created — unexamined.
- **Profile 9's gate/payload disagreement.** `presentation` declares it, `with_dolby_hdr_info`'s
  H265 guard silently drops the node. Costs nothing today (SDR-compatible base layer) and no such
  asset exists here, but the "gate and payload can never disagree" test does not cover it.
- **The 95-slot LUT ring is unlocked.** `DOVI_SWSync_Start` writes on the backend thread while
  `DOVI_SWSync_SetDoviLUTnMap` scans on the FW-comm thread, and the key sits at offset 0 of a
  0x71d0-byte `memcpy` into the slot. A lookup can match a slot whose LUT body is still being
  copied. Not our bug and not the one we fixed, but the same visible shape.
- **A `GetOTTMetaData` index bug in LG's code**: case 3 indexes the 200-entry MD ring with `% 95`,
  the LUT ring's size. Our path is case 1, so it does not bite us.
