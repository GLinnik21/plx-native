# Plex Native Playback Plan — direct-play everything the SM90 can decode

**Goal (verbatim):** "I want TV to play everything natively what it can on its HW and also
establish a proper communication by protocol with Plex server and also session that server
expects."

This is the single authoritative spec for: (1) the capability profile the client declares,
(2) the `/decision` handshake that replaces the hard-coded codec test, (3) the session +
timeline correlation that makes Now Playing correct, (4) HEVC/HDR10 direct-play through the
in-house demuxer + Starfish, (5) how soft subtitles + audio-track selection fold in once
everything direct-plays, (6) a phased, on-device-verifiable implementation plan, and (7) risks.

Everything below is reconciled against the live Rust code. The **live playback path** is
`route.rs` (hand-built query strings) + `player/*` (Starfish/ACB) + `stream.rs` (raw-socket
HTTP) + `mkv.rs` (demuxer). The typed `plex/*` layer is dead scaffold today (`plex::init` is
never called) — this plan migrates onto it where noted but does not block on it.

---

## Device decode envelope (the target we advertise)

LG 49SM9000PLA, α7 Gen2, webOS 4.5, in-app `StarfishMediaAPIs` BUFFERSTREAM.

| Class | Direct-play now (demuxer emits it) | Decoder-capable, gated on demuxer/probe | Never advertise |
|---|---|---|---|
| Video | H.264 High@L4.2 ≤1080p60 / L5.1 4K30 | HEVC Main/Main10 L5.1 4K60 (HDR10/HLG); VP9 Profile 0 | AV1; 10-bit AVC; VP9 Profile 2; Dolby Vision in-app |
| Audio | AC3, EAC3, AAC-LC | DTS core (`dca`) — but demuxer can't emit it → transcode | DTS-HD MA/HR; TrueHD |
| Subs | (none rendered yet) | SRT/ASS soft via demuxer (#soft-subs); PGS bitmap | — |
| Container | MKV (`V_MPEG4/ISO/AVC`, `V_MPEGH/ISO/HEVC`) | MP4/TS (demuxer is MKV-only) | — |

**Conservative stance:** HEVC 10-bit / HDR10 is decoder-capable per LG's 4.5 spec but the
in-app buffer-feed HDR path is *unverified* — Phase 0 probes it before we ship an HDR profile.
DTS/TrueHD/VP9-P2/AV1 always transcode to H264/AC3 MKV (the proven `start.mkv` path).

---

## 1. Capability profile — `X-Plex-Client-Profile-Extra`

### 1a. The string (URL-decoded, human-readable) — direct-play SDR set, shippable after Phase 2

```
add-direct-play-profile(type=videoProfile&container=mkv&videoCodec=h264,hevc&audioCodec=aac,ac3,eac3&subtitleCodec=srt,subrip,ass,ssa)+add-limitation(scope=videoCodec&scopeName=*&type=upperBound&name=video.width&value=3840&replace=true)+add-limitation(scope=videoCodec&scopeName=*&type=upperBound&name=video.height&value=2176&replace=true)
```

- `add-direct-play-profile(container=mkv…h264,hevc…aac,ac3,eac3…)` — server will direct-play
  an MKV whose video is H264/HEVC, audio is AAC/AC3/EAC3, subs SRT/ASS. (No `pgs`, no `vp9`,
  no `dca` until the demuxer renders/emits them — declaring a codec is a promise to render it.)
- Two `add-limitation … video.width=3840 / video.height=2176 (replace=true)` raise any base cap
  to 4K.
- **Deliberately no `video.bitDepth` limitation and no `video.colorTrc` match in the SDR set.**
  Omitting a bitDepth upper bound and adding no HDR match means a restrictive base could still
  transcode 10-bit — which is what we WANT until Phase 4 proves HDR decodes. The HDR unlock is
  a separate, additive directive block (§1c) gated behind the Phase 0 probe.

Base profile: send `X-Plex-Client-Profile-Name=Generic` (fully custom; not `Chrome`, whose base
has no HEVC direct-play and a 1080p/8-bit cap we'd fight).

### 1b. Percent-encoded (query-param form, via `pms::urlenc_str`, RFC-3986 unreserved only)

```
add-direct-play-profile%28type%3DvideoProfile%26container%3Dmkv%26videoCodec%3Dh264%2Chevc%26audioCodec%3Daac%2Cac3%2Ceac3%26subtitleCodec%3Dsrt%2Csubrip%2Cass%2Cssa%29%2Badd-limitation%28scope%3DvideoCodec%26scopeName%3D%2A%26type%3DupperBound%26name%3Dvideo.width%26value%3D3840%26replace%3Dtrue%29%2Badd-limitation%28scope%3DvideoCodec%26scopeName%3D%2A%26type%3DupperBound%26name%3Dvideo.height%26value%3D2176%26replace%3Dtrue%29
```

### 1c. HDR-unlock directives — APPEND (with `+`) only after Phase 0 proves HDR10 decodes

```
add-limitation(scope=videoCodec&scopeName=*&type=upperBound&name=video.bitDepth&value=10&replace=true)+add-limitation(scope=videoCodec&scopeName=hevc&type=upperBound&name=video.bitDepth&value=10&replace=true)
```

HDR10 = HEVC Main10 (10-bit) with BT.2020 + PQ signalled in-bitstream; direct play copies the
elementary stream verbatim, so HDR metadata passes through as long as no bitDepth/colorTrc
limitation is violated. The `scopeName=hevc` variant beats an HEVC-specific base cap (`replace`
matches on scope+scopeName). If HDR *still* transcodes, add the colorTrc match from the
capability-profile research §1c. **Do not ship 1c until the probe is green.**

### 1d. Where it attaches

- **Endpoint:** `GET /video/:/transcode/universal/decision` (adjudicate) and
  `…/start.mkv` (transcode fallback). Same param set on both.
- **Transport:** as a query param today (percent-encode the whole value, §1b). `stream.rs`
  supports real headers via `http_open`'s `extra` splice, but the decoded string may contain
  `& ( ) = + ,` freely as a *header* value — either works; keep query-param to match the existing
  path and `pms::urlenc_str`.
- **Required companions (else 400):** `X-Plex-Token`, `X-Plex-Client-Identifier` (stable device
  id), `X-Plex-Platform=Generic`, `X-Plex-Client-Profile-Name=Generic`,
  `path=%2Flibrary%2Fmetadata%2F{rk}`, `mediaIndex=0`, `partIndex=0`, `session`,
  `X-Plex-Session-Identifier`.

**Replaces** the current `route.rs:161-164` string
(`add-transcode-target(...videoCodec=h264&audioCodec=ac3)`) — keep that transcode target as a
*fallback* target but prepend the `add-direct-play-profile(...)` + `add-limitation(...)`
directives (joined by `+`) so the server may direct-play HEVC/4K and only transcode what's
outside the envelope.

---

## 2. Decision handshake — replace the `vcodec=="h264"&&acodec=="ac3"` heuristic

### 2a. The request (query-param form)

```
GET /video/:/transcode/universal/decision
  ?path=%2Flibrary%2Fmetadata%2F{rk}
  &mediaIndex=0&partIndex=0
  &protocol=http
  &hasMDE=1
  &directPlay=1&directStream=1&directStreamAudio=1
  &mediaBufferSize=20971
  &session={SESS}
  &X-Plex-Session-Identifier={SESS}
  &X-Plex-Client-Identifier={DEVICE_ID}
  &X-Plex-Product=Plex%20POC&X-Plex-Version=0.1.0
  &X-Plex-Platform=webOS&X-Plex-Platform-Version=4.5
  &X-Plex-Client-Profile-Name=Generic
  &X-Plex-Client-Profile-Extra={§1b blob}
  &X-Plex-Token={token}
Accept: application/json
```

`hasMDE=1` invokes the Media Decision Engine and requires real `mediaIndex`/`partIndex`
(not `-1`). `Accept: application/json` — else PMS returns XML.

### 2b. Parse the response → branch

The response is a `MediaContainer` with container-level decision codes and a rewritten
`Metadata[].Media[].Part[].Stream[]` tree where each `Part` and `Stream` carries a `decision`.

```
code = mdeDecisionCode  (fallback generalDecisionCode)
if no Metadata/Media item          -> server refused; read generalDecisionText/terminationText; abort
if code >= 2000                    -> not playable with these params (bandwidth/other); renegotiate/fail
locate selected Media -> selected Part; branch on Part.decision:

  "directplay"  -> FULL DIRECT PLAY.  GET the raw file at Part.key + ?X-Plex-Token=…
                   Read Media.videoCodec / Part.Stream[] codecs to configure demuxer + Load payload.
  "transcode"   -> inspect Stream[] decisions:
                     every Stream.decision == "copy"          -> REMUX / direct-stream (rewrap only)
                     any video Stream.decision == "transcode" -> FULL video transcode
                   -> GET /video/:/transcode/universal/start.mkv?{same base}&session={SESS}
                      (our start.mkv target stays H264/AC3; the demuxer already handles it)
  "none"        -> not playable; surface directPlayDecisionText/transcodeDecisionText
```

Decision codes: `1000` = direct play OK; `1001` = direct play unavailable, conversion OK
(1xxx = playable); `2xxx` = general error; `3xxx` = direct-play refused (read
`directPlayDecisionText` + `transcodeReasons` to learn *which* limitation blocked it — that
string tells you which `add-limitation(…replace=true)` to add); `4xxx` = transcode error.

**How forcing the profile makes HEVC/4K/HDR direct-play:** `directPlay=1` only says "I *can*";
the profile's `add-direct-play-profile(container=mkv&videoCodec=…hevc…)` + the width/height
(+ later bitDepth) upper bounds are what make the MDE return `Part.decision="directplay"` and
point `Part.key` at the raw file. A mismatch on *audio* or *subtitle* alone forces a whole-part
transcode even when the video could be copied — `directStreamAudio=1` lets audio copy while only
the incompatible stream is handled.

### 2c. Code changes

- Replace `build_stream` gate at `route.rs:240`
  (`let directplay = vcodec == "h264" && acodec == "ac3";`) with: issue the §2a `/decision`
  GET, **parse** the JSON (today `route.rs:250-251` discards the body via `http_get(...).None`),
  and branch on `Part.decision` per §2b. Keep the raw-part URL builder for `directplay`; keep
  the `start.mkv?{base}` builder for `transcode`.
- The migration target is `plex/transcoder.rs::transcode_decision` (returns a typed
  `ServerDecision`) — parse `Part.decision`/`Stream.decision` there and have `route.rs` call it.
  Until `plex::init` is wired, parse inline in `route.rs`.

---

## 3. Session + timeline — make Now Playing correct

### 3a. Root cause today

- Timeline (`player/threads.rs:229-256`) sends bare GETs with `X-Plex-Client-Identifier=
  com.beb.plxnative` and **no** `X-Plex-Session-Identifier`, no `playQueueItemID`, no
  `audioStreamID`/`subtitleStreamID`.
- Transcode (`route.rs:184`) uses `session=plxnative-{rk}` (per-ratingKey, not per-playback) and
  wrongly sets `X-Plex-Client-Identifier={session}`.
- The timeline and transcode session strings differ → PMS can't join them → dashboard shows the
  file's default track and mislabels Direct Play vs Transcode.

**Fix in one sentence:** one opaque session string per *playback*, used as BOTH the transcode
`session`/`X-Plex-Session-Identifier` AND the timeline `X-Plex-Session-Identifier`; a STABLE
device id in `X-Plex-Client-Identifier` on every request; a PlayQueue whose `playQueueItemID`
rides every timeline; and `audioStreamID`/`subtitleStreamID` on the timeline.

### 3b. Identity headers (send on every request incl. timeline)

| Key | Value | Notes |
|---|---|---|
| `X-Plex-Client-Identifier` | stable device id (persist once; e.g. `com.beb.plxnative` or persisted UUID) | groups the device; NEVER vary per item — fixes `route.rs:184` |
| `X-Plex-Session-Identifier` | fresh opaque id per Play | becomes `Session/@id`; MUST equal the transcode `session` param byte-for-byte |
| `X-Plex-Token` | token | already handled |
| `X-Plex-Product` | `Plex POC` | today `plxnative` |
| `X-Plex-Version` | `0.1.0` | today `1` |
| `X-Plex-Platform` | `webOS` | today `Generic` |
| `X-Plex-Platform-Version` | `4.5` | not sent today |
| `X-Plex-Device` | `webOS` / `LG TV` | device icon |
| `X-Plex-Device-Name` | e.g. `Living Room TV` | Now Playing name |
| `X-Plex-Model` | `49SM9000PLA` | cosmetic |
| `X-Plex-Provides` | `player` | treat as playback client |

### 3c. PlayQueue (recommended — first-class, remote-controllable session)

Progress/resume works without one, but official clients play through a PlayQueue. Cheap:

```
GET  /identity                       -> cache MediaContainer/@machineIdentifier (once at boot)
POST /playQueues?type=video
     &uri=server://{machineIdentifier}/com.plexapp.plugins.library/library/metadata/{rk}
     &continuous=1&shuffle=0&repeat=0
     &X-Plex-Client-Identifier={DEVICE_ID}&X-Plex-Session-Identifier={SESS}&X-Plex-Token=…
  -> playQueueID, playQueueSelectedItemID (= the playQueueItemID to report)
```

URL-encode the whole `uri` value. Keep `(playQueueID, playQueueItemID)` for the playback life.

### 3d. `/:/timeline` (POST — the spec verb)

```
POST /:/timeline
  ?ratingKey={rk}&key=%2Flibrary%2Fmetadata%2F{rk}&identifier=com.plexapp.plugins.library
  &state={playing|paused|buffering|stopped}&time={ms}&duration={ms}
  &playQueueID={pq}&playQueueItemID={pqi}
  &audioStreamID={a}&subtitleStreamID={s}     # s=0 => none; from selected Part.Stream[] ids
  &X-Plex-Session-Identifier={SESS}
  &X-Plex-Client-Identifier={DEVICE_ID}       # + identity block from §3b
  &X-Plex-Token=…
```

`audioStreamID`/`subtitleStreamID` are what make `/status/sessions` mark the right selected
stream **even on direct play** where there's no transcode decision to infer from. Cadence: every
~10s and immediately on state change (pause/resume/seek→buffering/track switch). `state=stopped`
commits the resume point + watched + On-Deck advance.

**Direct Play vs Transcode badge:** `/status/sessions` looks up the active transcode session
keyed by the client's session id. If a transcode session started with `session={SESS}` exists →
Transcode (with real source→target codecs from its `<TranscodeSession>`); else → Direct Play.
Same-string correlation is the entire fix.

### 3e. What `stream.rs` must gain

`http_open` already splices `extra` header lines verbatim (`stream.rs:136-147`) — the hook for
real headers exists. But `http_get`/`http_put` are the only wrappers and `http_put` hardcodes
`extra = NULL` (`stream.rs:341`). To send identity/session as *headers* (vs query params):
add an `extra: Option<&str>` param to the wrappers, or route all Plex calls through the typed
`plex::Client` (which builds `extra`). Also add a **POST** wrapper (or generalize `http_get`'s
method arg) — timeline is POST; today `threads.rs` GETs. Keeping them as query params (as
route.rs does now) is acceptable and lower-churn; the *values* are what matter, not the transport.

### 3f. Lifecycle summary

```
BOOT once:  GET /identity (cache mid); fix stable X-Plex-Client-Identifier
ON PLAY:    SESS = new opaque id
            POST /playQueues -> pqID,pqItemID
            (if transcode) PUT /library/parts/{part}?audioStreamID&subtitleStreamID  (server-side select)
            GET /decision?...&session=SESS&X-Plex-Session-Identifier=SESS  (register)
            stream: raw Part.key (directplay) OR start.mkv?...&session=SESS (transcode)
            POST /:/timeline state=playing time=0 + pq + streams + identity
DURING:     POST /:/timeline ~10s + on every state change
STOP/BACK:  POST /:/timeline state=stopped time=final
            (if transcode) GET /video/:/transcode/universal/stop?session=SESS&X-Plex-Client-Identifier=DEVICE_ID
```

Fix `stop_transcode` (`route.rs:95-111`) to pass `X-Plex-Client-Identifier={DEVICE_ID}`, not the
session string.

---

## 4. HEVC direct-play — demuxer + Starfish, byte-level

Reference: `mkv.rs` (demux), `player/engine.rs:15-16` (Load payloads),
`src/starfish.c`/`stub/starfish_stub.c` (mangled C++ seam), `route.rs:240` (gate).
HEVC/h265 appears **nowhere** today — the pipeline is hard-coded H264+AC3.

### 4a. `mkv.rs` — generalize the H264-specific pieces

**Reuse unchanged (codec-agnostic):** EBML primitives, `mkv_unlace`, block-header parse in
`mkv_handle_block`, the audio path (raw AC3/EAC3/AAC, `es=2`), the element-tree walk. The
Annex-B assembly loop (length-prefix → `00 00 00 01`) is codec-independent; only the
length-size source and NAL-type semantics are H264-specific.

**Changes:**

1. **Discriminant.** Keep `is_h264` (`mkv.rs:29`); **append** `is_hevc: c_int` (don't reorder —
   `#[repr(C)]`). Bump `sps_pps` to `[u8;2048]` and `SPS_CAP` (`mkv.rs:193`) to `2048` (HEVC
   VPS+SPS+PPS for 4K/HDR exceeds 1024).

2. **Codec-id detection** (after the `V_MPEG4/ISO/AVC` arm at `mkv.rs:560`):
   ```rust
   } else if cid.starts_with(b"V_MPEGH/ISO/HEVC") {
       (*c).is_hevc = 1;
       if cplen > 0 { mkv_parse_hvcc(c, &cp[..cplen]); }
   }
   ```
   CodecPrivate here is an `hvcC` record, not `avcC`.

3. **`mkv_parse_hvcc`** (new, model on `mkv_parse_avcc` at `mkv.rs:201`). HEVCDecoder-
   ConfigurationRecord fixed 23-byte prefix (ISO/IEC 14496-15 §8.3.3.1):
   - byte 0 = configurationVersion (==1; reject else).
   - **byte 21 low 2 bits → `nal_len_size = (p[21] & 0x03) + 1`** (the key divergence from
     avcC, which reads byte 4). Constraint flags are 48 bits/6 bytes (offsets 6–11) — that's
     why length-size/numArrays land at 21/22, not earlier.
   - byte 22 = numOfArrays; arrays begin at byte 23.
   - Each array: `byte0 = [completeness|reserved|NAL_type(&0x3F)]`, `u16 numNalus`, then
     `numNalus × (u16 nalUnitLength + NAL bytes)`. For each NAL emit `00 00 00 01` + bytes into
     `sps_pps` (reuse the avcc Annex-B writes). NAL types: **32=VPS, 33=SPS, 34=PPS** (keep in
     file order VPS,SPS,PPS). SEI arrays (39/40) may be skipped. Result: a ready-to-prepend
     VPS+SPS+PPS Annex-B blob, exactly analogous to the H264 SPS+PPS blob.

4. **Video branch** (`mkv.rs:434-507`). Gate on `is_h264 != 0 || is_hevc != 0` (`:434`). Select
   type-test + param-blob by the discriminant:
   - **NAL type (HEVC 2-byte header):** `let nt = (fd[i] >> 1) & 0x3f;`
     (vs H264 `fd[i] & 0x1f`).
   - **Keyframe/IRAP** — replace `nt == 5` (`:457`) with `let key = (16..=23).contains(&nt);`
     (BLA 16–18, IDR_W_RADL 19, IDR_N_LP 20, CRA 21, RSV_IRAP 22/23). At each IRAP prepend the
     VPS+SPS+PPS blob (same as H264 `:471-476`) and set the key flag (drives `nkey`/`aq_push
     es=1` at `:505` and the post-seek PTS rebase).
   The pass-1 scan, the Annex-B assembly loop (`:477-495`), and `aq_push(...es=1)` are
   byte-identical for HEVC. Keep the "skip laced video" guard (`:437-440`).

5. **Parse the `Video` (0xE0) element** (currently skipped at `mkv.rs:549`) for dimensions +
   HDR. In `mkv_parse_track_entry`, descend into 0xE0 and read (IDs verified vs CELLAR
   `ebml_matroska.xml`):

   | Element | ID | Use |
   |---|---|---|
   | PixelWidth / PixelHeight | 0xB0 / 0xBA | `esInfo.videoWidth/Height` |
   | Colour | 0x55B0 | HDR container |
   | MatrixCoefficients | 0x55B1 | `vui.matrixCoeffs` (CICP) |
   | Range | 0x55B9 | `vui.videoFullRangeFlag` (2 ⇒ full) |
   | TransferCharacteristics | 0x55BA | `vui.transferCharacteristics`; 16⇒HDR10, 18⇒HLG |
   | Primaries | 0x55BB | `vui.colorPrimaries` (CICP; 9=BT.2020) |
   | MaxCLL / MaxFALL | 0x55BC / 0x55BD | `sei.maxContentLightLevel` / `maxPicAverageLightLevel` |
   | MasteringMetadata | 0x55D0 | ST 2086 container |
   | PrimaryR/G/B Chroma X/Y | 0x55D1–0x55D6 (float) | `sei.displayPrimariesX/Y{0,1,2}` (R→0,G→1,B→2) |
   | WhitePoint X/Y | 0x55D7 / 0x55D8 (float) | `sei.whitePointX/Y` |
   | LuminanceMax / Min | 0x55D9 / 0x55DA (float) | `sei.max/minDisplayMasteringLuminance` |

   CICP values are ITU-T H.273 code points — same numbering LG's `vui` expects, pass through as
   integers. `frameRate`: derive from `DefaultDuration` (0x23E383, ns/frame) if present, else
   default 24000/1001 (or 60000/1001).

   Also confirm `scratch_cap` ≥ largest 4K AU (multi-MB IRAP AUs; a small scratch silently drops
   AUs at `mkv.rs:465/487`) and raise the `aq` byte-cap for 4K.

### 4b. Starfish `Load` payload — `player/engine.rs:15-16`

Replace the two fixed `const &str` payloads with a `format!`-built payload keyed off the item's
real vcodec + parsed dimensions (selection today at `engine.rs:163`). For HEVC + AC3:

```jsonc
"contents":{
  "codec":{"video":"H265","audio":"AC3"},        // LG name is "H265"; audio = detected AC3|EAC3|AAC
  "esInfo":{
    "pauseAtDecodeTime":false,"ptsToDecode":0,"seperatedPTS":true,
    "videoWidth":3840,"videoHeight":2160,          // from §4a.5
    "videoFpsValue":24000,"videoFpsScale":1001
  },
  "format":"RAW","provider":"plxnative"
}
```

Keep the `{"args":[{"mediaTransportType":"BUFFERSTREAM","option":{…}}]}` envelope,
`needAudio:true`, `transmission.contentsType:"LIVE"`. Set `adaptiveStreaming.maxWidth/maxHeight`
to real dimensions (3840×2160, not the hard-coded 1920×1080). `srcBufferLevelVideo.maximum`
already 8388608 (8 MiB, matches Kodi) — fine for 4K.

Also generalize `bf_split` (`engine.rs:94-105`): it splits AUs on the H264 AUD prefix
`00 00 00 01 09` — HEVC's AUD is NAL type 35 (`00 00 00 01 46 …`). Key the splitter off the
active codec.

### 4c. HDR10 — separate `setHdrInfo` call (NOT Load, NOT ACB)

On webOS HDR10 static metadata is delivered by `StarfishMediaAPIs::setHdrInfo(const char*)`,
called **after `Load()` reports LOADCOMPLETED and before `Play()`** (Kodi's `SetHDR`).

1. **Mangled symbol** `_ZN17StarfishMediaAPIs10setHdrInfoEPKc`:
   - `stub/starfish_stub.c`: `int _ZN17StarfishMediaAPIs10setHdrInfoEPKc(void){return 0;}`
   - `src/starfish.c` extern block (near `:37-49`):
     `extern int SMP_setHdrInfo(void*, const char*) __asm__("_ZN17StarfishMediaAPIs10setHdrInfoEPKc");`
   - New verb: `int sf_set_hdr(const char *msg){ return g_smp_ready ? SMP_setHdrInfo(g_smp,msg):0; }`
     (mirror in `starfish.h` + `player/ffi.rs`).

2. **HDR JSON** (Kodi shape; feed once between LOADCOMPLETED and `sf_play`):
   ```jsonc
   { "hdrType":"HDR10",            // Transfer 16→HDR10, 18→HLG, else skip the call
     "sei":{ "displayPrimariesX0..2/Y0..2":…, "whitePointX/Y":…,
             "minDisplayMasteringLuminance":…, "maxDisplayMasteringLuminance":…,
             "maxContentLightLevel":…, "maxPicAverageLightLevel":… },
     "vui":{ "transferCharacteristics":16,"colorPrimaries":9,"matrixCoeffs":9,"videoFullRangeFlag":false } }
   ```
   Unit conversions (Matroska float → LG int): chromaticity `round(v*50000)`; luminance
   `round(cd_m2*10000)` (send full int, not Kodi's u16-truncated form; A/B-test on panel);
   MaxCLL/MaxFALL as-is; `videoFullRangeFlag = (Range==2)`. Primary order R→0,G→1,B→2.
   Leave HDR10 static SEI in-band (harmless fallback). HDR10+/Dolby Vision out of scope.

### 4d. ACB — unchanged

`acb_bind → wait frames → acb_send_video_data(sourceInfoVerbatim) → acb_start`
(`pump.rs:129-154`, `starfish.c:102-113`) is codec-transparent — it forwards the pipeline's own
`sourceInfo` envelope verbatim. HEVC/4K/HDR need no ACB change.

### 4e. Direct-play gate (`route.rs:240`)

Once §2 lands, the gate is the `/decision` verdict. As an interim (pre-decision-parse) or a
fast-path, extend it to:
```rust
let directplay = (vcodec == "h264" || vcodec == "hevc")
              && matches!(acodec, "ac3" | "eac3" | "aac");
```

---

## 5. Subtitles + audio-track selection once everything direct-plays

**Explicit design point:** when a title direct-plays, the server hands us the raw MKV with all
tracks. Selection and rendering move *into the demuxer* — the server is no longer in the loop:

- **Audio-track selection happens in the demuxer** (#35): the demuxer picks which audio track's
  frames it feeds to Starfish. No `PUT /library/parts` re-encode, no re-transcode, no
  `retranscode`/`switch_audio` round-trip. We still report the chosen `audioStreamID` on the
  timeline (§3d) so Now Playing shows the right track.
- **Subtitles render soft** via the demuxer (SRT/ASS emitted as an `es=3` track and drawn by the
  UI text shader). **No burn.** The current transcode-time `subtitles=burn` path
  (`route.rs:transcode_base`) applies ONLY to the transcode fallback, not to direct-play.
- **PGS bitmap subs** stay out of the profile until the UI can overlay them — declaring `pgs`
  promises rendering; if we can't, the server would be forced to burn (⇒ transcode), which we
  don't want. Keep `subtitleCodec=srt,subrip,ass,ssa` only.

The `PUT /library/parts` server-side selection + `subtitles=burn` machinery (`route.rs:197-212`)
is retained solely for the transcode fallback path.

---

## 6. Phased implementation plan (each phase independently shippable + on-device verifiable)

No host runtime — verify via `/tmp/plxnative-events.log` (`make run` fetches it) + `tools/capture-
screen.sh [out.png] DISPLAY`. Dev triggers: `/tmp/plxnative-url` (override part URL), `/tmp/sample.h264`,
`/tmp/plxnative-autoplay`, `/tmp/plxnative-autoseek`.

### Phase 0 — HEVC + HDR10 buffer-feed probe (LOAD-BEARING; do first)

The single biggest unknown: **does `StarfishMediaAPIs` BUFFERSTREAM actually decode HEVC — and
then HDR10 — on THIS TV via ACB video-plane binding?** LG's 4.5 spec says the SoC can; the in-app
buffer-feed HDR path is undocumented. Prove it before building the demuxer around it.

- **Minimal probe:** hand-produce one short HEVC Annex-B elementary stream (e.g. an HEVC-in-MKV
  clip demuxed offline to `VPS+SPS+PPS` + a few IRAP AUs) and drop it on the TV as
  `/tmp/sample.h265`. Add a boot trigger (mirror the existing `/tmp/sample.h264` path) that Loads
  with a hand-written `"codec":{"video":"H265"}` payload (§4b) and feeds the AUs. Gate on
  `/tmp/plxnative-autoplay` for headless capture.
- **Files:** `player/engine.rs` (add `PAYLOAD_H265` const + a `/tmp/sample.h265` branch alongside
  the H264 sample), `stub/starfish_stub.c` + `src/starfish.c` (add `setHdrInfo` symbol),
  `bf_split` (HEVC AUD). No `mkv.rs`, no `/decision`, no session work yet.
- **On-device check:** `plxnative-events.log` shows LOADCOMPLETED + RECEIVE_GOOD_VIDEO (not
  ERROR_06/no-signal); `capture-screen.sh out.png DISPLAY` shows decoded HEVC frames on the video
  plane. Then feed an HDR10 clip + `sf_set_hdr(...)` and confirm the panel enters HDR (capture +
  visible tone) and no decode error.
- **Fallback if it fails:** HEVC does NOT direct-play on this TV → drop `hevc` from the §1a
  profile, keep HEVC on the transcode path forever, and skip Phases 3–4. If HEVC decodes but HDR10
  does not → advertise HEVC direct-play WITHOUT the §1c bitDepth unlock (SDR/8-bit HEVC direct,
  10-bit HDR transcodes). This is why §1a and §1c are split.

### Phase 1 — Session + timeline correctness (no codec work; pure protocol)

- **Files/fns:** `route.rs` (one `SESS` per playback next to `CUR_RK`; fix
  `X-Plex-Client-Identifier` to a stable device id at `:184`; fix `stop_transcode` `:95-111`);
  `player/threads.rs:229-256` + `engine.rs:310-317` (add `X-Plex-Session-Identifier`, PlayQueue
  ids, `audioStreamID`/`subtitleStreamID`, identity block; switch to POST); add `GET /identity`
  cache + `POST /playQueues` in `build_stream`. Enrich identity (product/version/platform/…).
- **Check:** `GET /status/sessions` on the server shows one session whose `Session/@id` ==
  our `SESS`, correct `Player` device name, correct selected audio/subtitle `Stream`, and correct
  Direct Play badge for the current H264/AC3 title.
- **Fallback:** if PlayQueue creation flakes, ship without it — bare timeline + shared `SESS`
  already fixes the badge + track display; PlayQueue is additive.

### Phase 2 — `/decision` handshake + capability profile (SDR, no new codec)

- **Files/fns:** `route.rs:build_stream` (parse the `/decision` JSON, branch on `Part.decision`
  per §2b — replace the `:240` heuristic); `transcode_base` profile string (§1a SDR blob, still
  no `hevc` demux yet so keep `videoCodec=h264` in the *direct-play* declaration until Phase 3,
  or advertise hevc but let Phase 0's verdict decide). Optionally route through
  `plex/transcoder.rs::transcode_decision` (requires wiring `plex::init` at `app.rs:194-199`).
- **Check:** `plxnative-events.log` logs the parsed decision (`Part.decision`, per-stream `decision`,
  `transcodeReasons`) for both an H264/AC3 title (→ directplay, raw part streams) and a
  known-transcode title (→ start.mkv). Behavior identical to today but now server-adjudicated.
- **Fallback:** if decision parsing is unreliable, keep the extended local gate (§4e) as the
  primary and use `/decision` only for its session-registration side effect (as today).

### Phase 3 — HEVC direct-play (SDR), demuxer + Load

- **Files/fns:** `mkv.rs` (§4a: `is_hevc`, `mkv_parse_hvcc`, HEVC NAL/IRAP in the video branch,
  `Video`/PixelWidth/Height parse, SPS_CAP→2048, scratch/aq caps); `engine.rs:15-16,163`
  (runtime `format!` payload, `"video":"H265"`, real dims); `bf_split` (HEVC AUD); `route.rs:240`
  gate (§4e) or the Phase-2 decision branch now enabling hevc; §1a profile now safely advertises
  `hevc`.
- **Check:** point `/tmp/plxnative-url` at a real HEVC 1080p/4K SDR MKV Part; `plxnative-events.log` shows
  hvcC parsed (VPS/SPS/PPS lengths), IRAP keyframes detected, feed stats healthy; capture shows
  decoded HEVC; `/status/sessions` shows Direct Play. Seek works (IRAP resync).
- **Fallback:** if hvcC/NAL parsing misbehaves on real files, gate `hevc` back out of the profile
  (server transcodes HEVC) while keeping the demuxer code behind the `is_hevc` discriminant.

### Phase 4 — HDR10 metadata pass-through

- **Files/fns:** `mkv.rs` (Colour/MasteringMetadata parse, §4a.5 lower half); `engine.rs`
  (build HDR JSON, call `sf_set_hdr` between LOADCOMPLETED and `sf_play`); `starfish.c`/stub/ffi
  (`setHdrInfo`, from Phase 0); §1c bitDepth-unlock directives appended to the profile.
- **Check:** real HDR10 4K MKV direct-plays; panel enters HDR (visible + capture); `plxnative-events.log`
  shows the HDR JSON fed and accepted; `/status/sessions` Direct Play. A/B the luminance unit
  forms.
- **Fallback:** if `setHdrInfo` is ignored/errors, leave HDR10 SEI in-band only (may still
  tonemap via decoder) or drop §1c so 10-bit transcodes; SDR HEVC (Phase 3) still ships.

### Phase 5 — Soft subs + in-demuxer audio-track selection (§5)

- **Files/fns:** `mkv.rs` (emit SRT/ASS as `es=3`; select audio track in demuxer per #35);
  UI text shader (draw subs); timeline reports chosen `audioStreamID`/`subtitleStreamID`.
- **Check:** switch audio track with no re-transcode (instant, `plxnative-events.log` shows demuxer
  track switch, not a `/decision`/start.mkv restart); soft SRT renders; `/status/sessions` shows
  the right track.
- **Fallback:** retain the existing `PUT /library/parts` + `subtitles=burn` transcode path for
  formats the demuxer/UI can't yet handle.

### Phase table

| Phase | Deliverable | Files/fns | On-device check | Fallback |
|---|---|---|---|---|
| 0 | HEVC+HDR10 buffer-feed probe | engine.rs sample-h265 branch, starfish setHdrInfo, bf_split | LOADCOMPLETED + RECEIVE_GOOD_VIDEO + capture; HDR panel | drop hevc/HDR from profile; transcode forever |
| 1 | Session/timeline correctness | route.rs SESS+device-id, threads.rs/engine.rs timeline, /identity, /playQueues | /status/sessions: right Session id, track, badge | ship without PlayQueue |
| 2 | /decision + profile (SDR) | route.rs build_stream parse+branch, transcode_base profile | log parsed decision; both DP + transcode titles work | keep local gate, decision for registration only |
| 3 | HEVC direct-play (SDR) | mkv.rs hvcC/NAL/dims, engine.rs H265 payload, bf_split | real HEVC MKV decodes + Direct Play + seek | gate hevc out of profile |
| 4 | HDR10 pass-through | mkv.rs Colour, engine.rs setHdrInfo, §1c profile | HDR panel + Direct Play | SEI in-band only / drop §1c |
| 5 | Soft subs + audio select | mkv.rs es=3 + track select, UI shader | instant audio switch, soft subs | keep burn/PUT fallback |

---

## 7. Risks & open questions

1. **[TOP] Does buffer-feed HEVC + HDR10 decode on this exact TV?** Undocumented for in-app
   BUFFERSTREAM. De-risk: Phase 0 probe before any demuxer work. Everything HEVC/HDR is gated on
   its verdict; the profile is split (§1a SDR / §1c HDR) precisely so we can ship the largest
   subset that actually works.
2. **LG codec string** — `"H265"` (Kodi's `ms_codecMap`) vs `"HEVC"`/`"video/H265"`. Verify in
   Phase 0 by trying `"H265"` first; if LOAD fails, try `"HEVC"`. Single JSON field, cheap to A/B.
3. **`setHdrInfo` luminance units** — Kodi truncates `maxDisplayMasteringLuminance` to u16
   (overflow ≥6.55 nits). Plan sends the full integer; A/B against the cd/m² form on-panel
   (Phase 4).
4. **4K AU sizes** — multi-MB IRAP AUs can silently overflow `scratch_cap`/`aq` byte-cap
   (`mkv.rs:465/487`) and drop AUs. De-risk: raise caps + assert-log oversize AUs in Phase 3.
5. **`#[repr(C)]` MkvCtx layout** — append `is_hevc`, never reorder, or any C mirror desyncs.
6. **Decision-code coverage** — only `1000`/`1001` are confirmed from `serverdecision.py`; the
   `3xxx`/`4xxx` specifics are forum-sourced, not an enumerated table. De-risk: branch on
   `Part.decision` (exact enum from the OpenAPI schema), treat codes as advisory, log
   `transcodeReasons` verbatim.
7. **Audio-only-mismatch forcing whole-part transcode** — if a file's audio/subs fall outside the
   profile, the video won't direct-play even if decodable. `directStreamAudio=1` mitigates; the
   §1a audio set (aac/ac3/eac3) covers the common cases; DTS still transcodes.
8. **Typed `plex/*` layer is dead** (`plex::init` never called). Wiring it is optional for every
   phase; if it churns too much, keep inline route.rs parsing and migrate later.
9. **`stream.rs` is IP-only, no DNS, no chunked-request, PUT hardcodes NULL extra** — session
   headers as query params sidestep this; if we move to real headers, extend the wrappers first.
10. **Dolby Vision / HDR10+ / VP9-P2 / DTS-HD / TrueHD / AV1** — explicitly out of scope; always
    transcode. Never advertise (would promise a render/decode path we don't have).
