# URI ACB video-data envelope: the LS2 `videoInfo` event and the firmware's own conversion, 2026-11-05

The production URI backend binds the video plane through ACB on a 4.x set (`Binding::Acb`,
[the shared sequence](../uri-mode.md)). One step of that sequence hands ACB a "video-data" envelope
(`setMediaVideoData`). Until this measurement the backend forwarded only an envelope the LS2 service
itself sent, and on the webOS 4.5 dev set the service never sends one. This note records what it sends
instead, where the firmware's own ACB envelope comes from, and the field map the backend now applies.

## What the 4.5 service sends

Captured from the app's own subscription to `com.webos.media` on the dev set, for a 720p24 H.264 + AAC
clip played through the URI route (messages in the order seen; the media id is the service's opaque
connection id, kept as captured):

```
{"videoInfo":{"width":1280,"height":720,"aspectRatio":"16:9","frameRate":24,"bitRate":120,"mode3D":"2d","actual3D":"2d","scanType":"progressive","pixelAspectRatio":"1:1","afd":16,"rotation":0,"userData":"userData=(null)","specificRendering":"none","hfr":false,"hdrType":"none","SEI":{"displayPrimariesX0":0,"displayPrimariesX1":0,"displayPrimariesX2":0,"displayPrimariesY0":0,"displayPrimariesY1":0,"displayPrimariesY2":0,"whitePointX":0,"whitePointY":0,"minDisplayMasteringLuminance":0,"maxDisplayMasteringLuminance":0,"maxContentLightLevel":0,"maxPicAverageLightLevel":0},"VUI":{"transferCharacteristics":2,"colorPrimaries":2,"matrixCoeffs":2},"mediaId":"_M7yz1mOAwtVmU7"}}
{"sourceInfo":{"numPrograms":1,"seekable":true,"trickable":true,"startDate":-1,"rotation":0,"programInfo":[{"duration":90000,"numAudioTracks":1,"audioTrackInfo":[{"bitRate":0,"codec":"aac","profile":"lc","level":2,"sampleRate":48,"channels":1,"pid":257,"ctag":-1,"audioType":0,"trackId":-1,"immersive":"none","periodStart":0}],"numVideoTracks":1,"videoTrackInfo":[{"angleNumber":0,"codec":"h264","profile":"high","level":4.1,"frameRate":24,"pid":256,"ctag":-1,"trackId":-1,"orgCodec":"h264","width":1280,"height":720,"periodStart":0}],"numSubtitleTracks":0,"subtitleTrackInfo":""}],"mediaId":"_M7yz1mOAwtVmU7"}}
{"loadCompleted":{"mediaId":"_M7yz1mOAwtVmU7"}}
```

The order was `audioInfo`, `bufferRange`, `videoInfo`, `sourceInfo`, `loadCompleted`: the video facts
arrive before `loadCompleted`. Two things matter:

- `mediaId` sits INSIDE each event object, not beside it.
- The LS2 `sourceInfo` is a track list (programs, audio and video track info). It has no string
  `context` and no `video` object, so it is not the ACB envelope schema the backend used to wait for,
  and the bind logged `uri-acb: waiting for the service's source envelope (real video facts)` until the
  start timed out and the route fell back to Feed.

## Where the firmware's ACB envelope comes from

Decompile of `libplayerAPIs.so.1.0.0` from the 4.5 set's firmware (webOS 4.10.2, `HE_DTV_W19H_AFADABAA`,
SHA-256 `5b5252c5984764760e29977ff12bc92125ae091b55de4dc56d2870cd6f67a4b3`; addresses below are ELF
virtual addresses, Ghidra shows them +0x10000). `libpf`, `libAcbAPI` and the ACB service were not
decompiled.

libplayerAPIs performs no LS2-to-ACB conversion. Two builders read the SAME member fields of one
`StarfishMediaAPIs` object, and the constructor's uid decides which one runs:

- Pipeline-process mode (uid set): the callback hook forwards the pipeline's raw video line and
  `StarfishMediaPipe::sendVideoInfoEvent` (0x4ae50) builds the LS2 `videoInfo` event, the message above.
- App-process mode (uid NULL, which is how our Starfish seam constructs it, `src/starfish.c`): the
  type-4 callback case (0x37d58) parses the same line (`updateVideoInfo`, 0x2f4b8), then
  `generateVideoInfoPtree` (0x36fe4) and `smp::util::generateJsonString` (0x4f618) write the
  `{context, content, video}` JSON that Feed forwards to ACB. The builder runs on every type-4 callback
  whose parsed video struct or HDR info changed. libplayerAPIs itself never calls ACB.

So the envelope Feed binds with in production and the LS2 `videoInfo` are two spellings of the same
members. The backend now writes the first from the second.

## The field map

Builder 0x36fe4 unless noted. "Proven" means the decompile shows the rule; "inferred" is called out.

| Envelope key | LS2 `videoInfo` member | Rule | Evidence |
| --- | --- | --- | --- |
| `context` | `mediaId` | the connection id (`getMediaID`, 0x2d72c) | call proven; equality with the LS2 id inferred from naming and logs |
| `content` | none | the constant `"movie"` | 0x36fe4 |
| `video.frameRate` | `frameRate` | the same float member, copied | 0x36fe4, 0x4ae50 |
| `video.scanType` | `scanType` | `"progressive"` becomes `VIDEO_PROGRESSIVE`; anything else `VIDEO_INTERLACED` (LS2 derives its string from the same test) | 0x36fe4, 0x4ae50 |
| `video.width`, `video.height` | `width`, `height` | verbatim | 0x36fe4 |
| `video.bitRate` | `bitRate` | verbatim: the same member (unit not determined) | 0x36fe4, 0x4ae50, 0x2f4b8 |
| `video.adaptive` | none | true iff the `load` carries `option.adaptiveStreaming` or a `mediaTransportType` of WIDEVINE, MSIIS, HLS or MPEG-DASH (`parseLoadMessage`, 0x345a4) | rule proven; transport-type map partly folded by the compiler |
| `video.path` | none | `"network"` when the pipeline-side flag is 1, else `"file"`; initialised to 1 | rule proven; the value for an http source is inferred |
| `video.pixelAspectRatio` | `pixelAspectRatio` `"W:H"` | `{width: W, height: H}`; LS2 writes `"%d:%d"` (0x4d810) | 0x36fe4, 0x4d810 |
| `video.afd` | `afd` | verbatim | 0x36fe4 |
| `video.rotation` | `rotation` | emitted as a number | 0x36fe4, 0x4f618 |
| `video.data3D.originalPattern`, `.currentPattern` | `mode3D` | both are one lookup (`find3DInfoString`, 0x4dd7c); LS2 uses a second table (`find3DUMSInfoString`, 0x4dcb4) | 0x36fe4, 0x4dd7c, 0x4dcb4 |
| `video.data3D.typeLR` | `mode3D` | `"LR"`, or `"RL"` for the 3D modes 2, 4 and 9 | 0x36fe4, 0x2f4b8 |
| `video.hfr` | `hfr` | written only while a per-instance latch is clear, i.e. in the first envelope of an instance | 0x36fe4, `isHFR` 0x2f6c4 |
| `video.hdrType` | `hdrType` | `getHdrTypeString` (0x4dd18), the same table LS2 uses (none, HDR10, DolbyVision, VP9, HLG, Prime) | 0x36fe4, 0x282f4 |
| `video.mediaSei` | `SEI` | the same tree under a renamed key; each of the 12 members only when its value is non-negative; the key only when non-empty | 0x4e334 |
| `video.mediaVui` | `VUI` | the same tree under a renamed key; `videoFullRangeFlag` only for HLG; the key only when non-empty | 0x4e664 |

Written in this order: `context`, `content`, then `video` with `frameRate`, `scanType`, `width`, `height`,
`bitRate`, `adaptive`, `path`, `pixelAspectRatio`, `afd`, `rotation`, `data3D`, `hfr` (conditional),
`hdrType`, `mediaSei` (conditional), `mediaVui` (conditional). Not in the ACB envelope at all:
`specificRendering`, `userData`, `aspectRatio`, `actual3D`, `codec`, the Dolby and HDR10+ blocks, profile
and level. Strings that look like `null`, `true`, `false` or a number are written unquoted.

The 3D tables (0x839bc and 0x83a34), by firmware mode number: 0 is `2d`; 1 and 2 are half-width
side-by-side (LS2 `side_side_half_LR` and `_RL`); 3 and 4 are top-and-bottom (LS2 `top_bottom` and
`bottom_top`); 5 `check_board`; 6 and 11 `frame_squential` (the firmware's own spelling); 7
`column_interleave`; 8 and 9 full-width side-by-side (LS2 `side_side_full_LR` and `_RL`); 12
`line_interleave_half`; 13 `dual_stream`; 10 and 14 have empty names. The envelope's pattern drops the
`_LR`/`_RL` suffix and names top-and-bottom `top_bottom` for both 3 and 4.

## What the backend does with it

`rust-modules/play-uri/src/acb_envelope.rs` holds the map as pure functions.

- It takes a `videoInfo` object only when its own nested `mediaId` is the loaded media's (the lifecycle's
  filter, the same one that guards a service-sent envelope).
- It copies the service's numbers and strings as given, and writes only the firmware's own constants
  (`content`, `path`) itself. `adaptive` is the firmware rule applied to the `load` the backend sent
  (`AbrBps::is_adaptive`: the production `load` carries `adaptiveStreaming` only when rates were asked for,
  and never a `mediaTransportType`), so it is `false` in production.
- It FAILS CLOSED: no envelope when a member the firmware always writes is missing or has another type
  (`mediaId`, `width`, `height`, `frameRate`, `scanType`, `bitRate`, `pixelAspectRatio`, `afd`, `rotation`,
  `mode3D`, `hdrType`), when `width`, `height` or `frameRate` is not positive, when `pixelAspectRatio` is not
  two digit runs around a colon, or when `mode3D` is not one of the 12 table spellings above (the empty
  slots of modes 10 and 14 included). `hfr`, `SEI` and `VUI` are conditional in the firmware too, so they
  are left out when the service sent none or an empty one. The bind then keeps waiting and times out as it
  did before.
- An envelope the service sends in the ACB schema (a `sourceInfo` with a string `context` and a decodable
  `video`) is still preferred to a derived one, in either arrival order.

For the capture above the backend sends (key order as the firmware writes it):

```
{"context":"_M7yz1mOAwtVmU7","content":"movie","video":{"frameRate":24,"scanType":"VIDEO_PROGRESSIVE","width":1280,"height":720,"bitRate":120,"adaptive":false,"path":"network","pixelAspectRatio":{"width":1,"height":1},"afd":16,"rotation":0,"data3D":{"originalPattern":"2d","currentPattern":"2d","typeLR":"LR"},"hfr":false,"hdrType":"none","mediaSei":{"displayPrimariesX0":0,"displayPrimariesX1":0,"displayPrimariesX2":0,"displayPrimariesY0":0,"displayPrimariesY1":0,"displayPrimariesY2":0,"whitePointX":0,"whitePointY":0,"minDisplayMasteringLuminance":0,"maxDisplayMasteringLuminance":0,"maxContentLightLevel":0,"maxPicAverageLightLevel":0},"mediaVui":{"transferCharacteristics":2,"colorPrimaries":2,"matrixCoeffs":2}}}
```

(`serde_json`'s `preserve_order` is not enabled anywhere in the workspace's feature graph
(`cargo tree -e features -i serde_json`), so the text on the wire has the same members sorted by key,
which is also how the pinned descriptor ACB accepted was written; nothing known about the firmware's
reader depends on order.)

## Not determined

1. Whether ACB accepts this envelope: its acceptance rules (the ACB service, `libAcbAPI`) were not
   decompiled, and the earlier pinned descriptor ACB did accept differs from it (it carries
   `specificRendering`, a string `rotation`, a `videoFullRangeFlag` of `false` and no `afd`). Numeric versus
   string `rotation` is the open question there. Only a run on the set settles it.
2. The unit of `bitRate`, and the value `path` takes for an http source (both copied or constant, as the
   firmware does).
3. That `context` equals the LS2 `mediaId` (the call is proven, the equality is by naming and logs).
4. That the firmware builds exactly one envelope per LS2 `videoInfo` (inferred), and whether anything but
   the first envelope clears the `hfr` latch.
5. The float precision of a non-integer `frameRate` such as 23.976.

Host tests pin the map, the fail-closed cases and the backend's bind on this capture
(`plx_play_uri`: `acb_envelope`, `backend`). The run on the television that confirms ACB takes the
envelope and moves video is still to be done.
