# URI mode (the television's own media pipeline): what each firmware has answered

"URI mode" here means handing the stream URL to *something other than our own feeder* and letting
a platform pipeline fetch, demux, decode and render it. Two topologies do that, and they are not
the same question:

| topology | who owns the pipeline | binding route |
|---|---|---|
| **in-process** | the app's own process, through `StarfishMediaAPIs` (`libplayerAPIs`), with the URL or with fed buffers | ACB on 4.x, the exported window on 5+ |
| **out-of-process** | `com.webos.media` (`umediaserver`) spawns `starfish-media-pipeline` | exported window (`option.windowId`) on 5+; controlled ACB binding demonstrated on the 4.5 dev set |

Our normal playback is in-process and feeds buffers (`docs/buffer-feed-plan.md`). The probe below
examines the **out-of-process** route to let the firmware fetch and demux media our own feeder
cannot handle. LG's own web apps are in-process too — `libcbe.so`
has `DT_NEEDED libplayerAPIs.so.1` and `libAcb.so.1` and imports 19 `StarfishMediaAPIs::*` and 22
`Acb::*` symbols on webOS 4.10.0, and 23 `StarfishMediaAPIs` imports on 33.31.68. They play a
`<video src>` URL through their own copy of the same library and bind its plane with ACB.
That topology does not prohibit out-of-process binding: the controlled 2026-11-02 ACB experiment
displayed its URI fixture on the 4.5 dev set, as recorded below.

## Results

| firmware | set | result |
|---|---|---|
| webOS 4.10.0 / 4.4.2 | unpacked images + this project's 4.5 set | No `AVProxy` or `SDL_webOS*ExportedWindow`; the window option is ignored. Earlier probe loads decoded without a visible picture, and earlier ACB attempts returned `ERROR_06`. The controlled 2026-11-02 ACB pair later displayed moving SDR720p24 H.264/AAC URI video on the 4.5 dev set, both with and without `connectDass`; this does not establish other 4.x sets or formats. |
| webOS 5.4.2 | LG 65UN80006LA (issue #1064) | `windowId` in `load` → picture, moving, with sound. `late` (`setWindowId` after `loadCompleted`) and `none` → `error code=104 "fail to create AVProxy"`. Both a `plex.direct` HTTPS direct play and an HLS transcode played. Where the picture sat and its aspect were not reported (the tester's report names picture and sound only), and the registration ladder ended on the named client (`shape=named`). |
| webOS 5.6.2 (build 5.6.2-21) | LG OLED CX (issue #1064: [the first run](https://github.com/plxnative/plx-native/issues/1064#issuecomment-6758688148), [the single-audio product run and drive probe](https://github.com/plxnative/plx-native/issues/1064#issuecomment-6844822550)) | **Admitted: major 5 is in `EXPORTED_STRATA`.** (a) The 5.5.0 and 5.6.2 images read `option.windowId` at load (decompile, below). (b) The PRODUCT run, on the CI build of run 43017142892 (main at `e8442d5e`, which carries #1314) installed over the existing debug app, Playback engine Experimental, quality Original, `plxnative-uriexported` armed at the jail's persistent path: a 1080p HEVC (1920x1040) MKV with ONE AAC 2.0 track and no subtitle streams, direct play. Every boot: `uri: product binding=Unbound mode=2 handle=false ptype=10 initialized=false major=5 evidence=none`, then `uri: product binding=Exported { canvas: (1920, 1080) } mode=2 evidence=override`. Play: `ls2uri: register shape=named ok after 6ms`, `uri: exported window placed src=1920x1040 rv=1`, `uri: audio index=0 is already selected (audioInfo); no selectTrack sent`, `uri: service playing`. The picture moved, full width with thin bars for 1.85:1, not stretched. Back: `ls2uri: released`. (c) The drive probe on the same build printed `load=ok playing=yes ... currentTime_advanced=yes videoInfo=seen audioInfo=seen window=exported ... shape=named end=script audio_at_load=0 clock_period_ms=200 pause=froze pause_delta=- resume=sent seek=hit seek_delta=-3898 text=sent audio=sent subs=ok eos=no pause_basis=paused_event_and_resume pause_samples=0 ... paused_event=yes seekdone=yes resume_from=2568`. `pause=froze` is printed by the current grader (#1314), so no amendment is used for it. The tester gives no paused position for the probe (no `drive pause pos=` line), so `resume_from=2568` is not compared here against rule (c)'s paused position minus 1000 ms; `pause_basis=paused_event_and_resume` is the grader's own check of the resume against the position it recorded. The audio token is `audio=sent` with `audio_at_load=0`, the rule's `audio_at_load=` alternative. Product controls: pause logged `ev=paused` and two service screenshots 2 s apart were identical (mean pixel difference 0.007 of 255); Play resumed at 39383 ms, where it paused (`timeline paused t=39s`); seek: `uri: control Seek(ContentTime(65606)) landed on the service's seekDone (its reply acknowledged first: yes)`, first `currentTime` after it 62692; a listener in the room confirmed sound in a second play: throughout, silent during a 5 s pause, back on resume and after a seek. Extra datapoint: a 4K HEVC Dolby Vision profile 8.6 MKV (3840x1604, one AC3 5.1 track) showed the TV's Dolby Vision badge, letterboxed for 2.39:1, with the same pause, seek and sound. The multi-audio film now starts in Standard and logs `uri: original declined (several audio tracks on the exported window)`; `exported_uri_leaves_multi_audio_originals_on_standard` (`rust-modules/src/app/adapters/backends.rs`) pins that route. History: the first run (a debug build of `main` at 5011614a, before #1314) printed `pause=unconfirmed` from its drive probe (no `currentTime` while paused, so no samples to grade), and its product run of that four-track file (AAC 5.1, AAC 5.1, Vorbis 5.1, Vorbis 5.1) failed the audio-track switch: the service lists the tracks as AAC, Vorbis, AAC, Vorbis, so `uri: selectTrack sent kind=audio index=1 stream=364534` selected Vorbis, the bus answered `{"error":{"errorCode":200,"errorText":"Audio Codec Not Supported"}}` and the play fell back to Standard. That is rule (c)'s amendment (2) case: such a file now starts in Standard, and the admission covers only the route as it now stands (audio switching on the URI path is avoided, not admitted). Not checked: subtitles on the URI path, HLS. |
| webOS 6.5.3 (firmware major 6) | unpacked image `HE_DTV_W21O_AFABATAA` (webOS TV 6.5.3, release 6.5.3-47); LG OLED C1 (6.5.3-47), a Dolby Vision run on 2026-11-17 | **Admitted, bracketed, no product run: major 6 is in `EXPORTED_STRATA`.** (a) The 6.5.3 image reads `option.windowId` at load: `libplayerAPIs.so.1.0.0` (sha256 prefix `0b0f6654`), `StarfishMediaAPIs::parseLoadMessage`. The string `option.windowId` (file offset 0x874bc) is referenced at 0x41556 and is read as an optional string from the load JSON; when the key is present the function stores it at `MediaApiPrivate+0x90` and passes it to a virtual call (vtable slot +0x74 of the object at `MediaApiPrivate+0x5c`; the 11.2 read names the corresponding call (slot +0x78 of `+0x60`) the AVProxy builder, and the slot was not separately identified on this image). The same image's string table has `fail to create AVProxy` (0x88b44), the error 104 text. Addresses are file VAs (`objdump -d`; Ghidra's are +0x10000). Bracketed: majors 5 and 10 are each admitted on a tester's product run (the rows above and below), and they are the nearest major admitted on tester evidence on each side of 6. No product run exists on a 6.x set, so there is no (b) and no (c): the Dolby Vision run below shows a picture and seeks, but nothing says sound, pause or an audio switch works there, and no other 6.x release has been read. The boot line reads `major=6 evidence=table` like any admitted major's; it does not say a tester ran it. Dolby Vision run, 2026-11-17, LG C1 (6.5.3-47): an earlier revision of #1399 that put Standard's `DolbyHdrInfo` node on the URI Load, Playback engine Experimental, quality Original, a 4K HEVC Dolby Vision Profile 8 MKV (one audio track, no subtitle streams), three plays paused on the same three frames, each captured with `com.webos.service.capture` (DISPLAY, after the picture processing) beside the settings service's `dynamicRange` / `pictureMode`. (A) URI with the node: `dolbyHdr` / `dolbyHdrCinemaBright`, `videoInfo hdrType=DolbyVision`, the panel's Dolby Vision badge; (B) URI without it: `hdr` / `hdrCinemaBright`, `hdrType=HDR10`; (C) Standard: `dolbyHdr` / `dolbyHdrCinemaBright`. Mean luma / peak: A 67.9/168, 40.1/131, 26.0/239; B 77.5/209, 43.2/157, 27.8/254; C 78.2/208, 44.8/153, 27.1/239. A is 7-14% darker than both: the node switched the set into its Dolby Vision mode over the base-layer decode, as the Matroska read below predicts. Seeks: 9 across A and B, every one landed on `seekDone`. On 2026-11-18 a stable build with the decline logged the decline for the same file (in its earlier wording, `a Dolby Vision MKV before webOS 10`), which then played on Standard as Dolby Vision (the panel's badge and picture mode). This is not a product run under rule (b): nothing was checked for sound, pause or an audio switch. |
| webOS 7.6.0 (firmware major 7) | unpacked image `HE_DTV_W22P_AFADATAA` (webOS TV 7.6.0, release 7.6.0-1901); no tester run | **Admitted, bracketed, no tester run: major 7 is in `EXPORTED_STRATA`.** (a) The 7.6.0 image reads `option.windowId` at load: `libplayerAPIs.so.1.0.0` (sha256 prefix `0969903e`), `StarfishMediaAPIs::parseLoadMessage`. The string `option.windowId` (file offset 0x971d8) is referenced at 0x467c6 and is read as an optional string; when the key is present the function stores it at `MediaApiPrivate+0x90` and passes it to a virtual call (vtable slot +0x74 of the object at `MediaApiPrivate+0x5c`, not separately identified on this image). The same image's string table has `fail to create AVProxy` (0x98b9c). Addresses are file VAs. Bracketed: majors 5 and 10 are each admitted on a tester's product run, and they are the nearest major admitted on tester evidence on each side of 7. No tester run exists on a 7.x set, so there is no (b) and no (c), and no other 7.x release has been read. The boot line reads `major=7 evidence=table`. |
| webOS 8.3.1 (firmware major 8) | unpacked image `HE_DTV_W23O_AFABATAA` (LG OLED65C3PUA, firmware 03.33.25, webOS TV 8.3.1, release 8.3.1-42); no tester run | **Admitted, bracketed, no tester run: major 8 is in `EXPORTED_STRATA`.** (a) The 8.3.1 image reads `option.windowId` at load: `libplayerAPIs.so.1.0.0` (sha256 prefix `e5ab26e1`), `StarfishMediaAPIs::parseLoadMessage`. The string `option.windowId` (file offset 0x972d0) is referenced at 0x47014 and is read as an optional string; when the key is present the function stores it at `MediaApiPrivate+0x90` (0x4704e) and passes it to a virtual call (0x47068: vtable slot +0x78 of the object at `MediaApiPrivate+0x5c`, which the `MediaApiPrivate` constructor (0x3d3d0) makes a `subtitle::SubtitleTask`; on this image the slot is its `setLayerInfo`, called with the window id as its first argument). `createAVProxy` copies the same string from `MediaApiPrivate+0x90` into the `AVProxy::Builder` it builds the AVProxy from (0x3e3aa). The same image's string table has `fail to create AVProxy` (0x98e18, referenced at 0x52f82 beside the error code 104), and its `starfish-media-pipeline` (`0df4d5a4`) reads the option the same way. Addresses are file VAs. Bracketed: majors 5 and 10 are each admitted on a tester's product run, and they are the nearest major admitted on tester evidence on each side of 8. No tester run exists on an 8.x set, so there is no (b) and no (c), and no other 8.x release has been read. The boot line reads `major=8 evidence=table`. Dolby Vision on this image: the "Dolby Vision on a URI Load" row below. |
| webOS 9.2.4 / 10.3.1 (33.31.68) | unpacked images; LG C2 on webOS 25 (10.x, 33.31.68; issue #1064: [the set](https://github.com/plxnative/plx-native/issues/1064#issuecomment-6507267775), [the drive probe](https://github.com/plxnative/plx-native/issues/1064#issuecomment-6749905096), [the first product run](https://github.com/plxnative/plx-native/issues/1064#issuecomment-6806253745), [the Dolby Vision product run](https://github.com/plxnative/plx-native/issues/1064#issuecomment-6834626267), [a DTS file](https://github.com/plxnative/plx-native/issues/1064#issuecomment-6834728153), [the wider-than-16:9 product run](https://github.com/plxnative/plx-native/issues/1064#issuecomment-6884620557)) | **Admitted: major 10 is in `EXPORTED_STRATA`, on tester evidence. Major 9 is admitted too, bracketed, no tester run.** Major 9, (a): the 9.2.4 image (`HE_DTV_W24G_AFABATAA`, webOS TV 9.2.4, release 9.2.4-48) reads `option.windowId` at load: `libplayerAPIs.so.1.0.0` (sha256 prefix `cc08e2b8`), `StarfishMediaAPIs::parseLoadMessage`. The string `option.windowId` (file offset 0x9a7c0) is referenced at 0x4385c and is read as an optional string; when the key is present the function stores it at `MediaApiPrivate+0x94` and passes it to a virtual call (vtable slot +0x78 of the object at `MediaApiPrivate+0x60`, not separately identified on this image). The same image's string table has `fail to create AVProxy` (0x99884). Addresses are file VAs. Bracketed: majors 5 and 10 are the nearest major admitted on tester evidence on each side of 9. No run exists on a 9.x set, so there is no (b) and no (c) for major 9, and no other 9.x release has been read. For major 10: (a) The 10.3 image (33.31.68) reads `option.windowId` at load (decompile, below). (b) The PRODUCT run of 2026-11-14 ([the tester's comment](https://github.com/plxnative/plx-native/issues/1064#issuecomment-6884620557)), on a debug build the tester does not name. It answers the maintainer's request of 2026-11-13 for the CI build of run 43421880266 (main at `2d565dc9f`, #1331's merge) with the override armed, Playback engine Experimental, quality Original and an original wider than 16:9 with one audio track and no subtitles. It opens "Everything was successful again: aspect ratio correct, picture is full width with black bars, audio is playing, pause, resume, seek working." and pastes the event log's `grep -E 'uri(probe)?: |ls2uri: '`: boot `uri: product binding=Exported { canvas: (1920, 1080) } mode=2 evidence=override` (the paste has no `uri: product binding=Unbound ... major=10 evidence=none` line before it, which the earlier Dolby Vision paste had, so no line of this paste names the firmware major: the run is attributed to this row by the tester's own answer in the thread, on their set); `uri: original declined (a trailer preview)`; `ls2uri: register shape=named ok after 2ms`; `uri: exported window placed src=1920x1080 rv=1`, then `uri: exported window placed src=3840x1608 rv=1` (3840x1608 is 2.39:1); `uri: audio index=0 is already selected (audioInfo); no selectTrack sent`; `uri: service playing`; `uri: control Seek(ContentTime(54901)) landed on the service's seekDone (its reply acknowledged first: yes)` and `uri: control Seek(ContentTime(130201)) landed on the service's seekDone (its reply acknowledged first: yes)`; `ls2uri: released`. This run meets rule (b): the override boot line; a picture placed from a 2.39:1 source, "full width with black bars" in the tester's words; sound ("audio is playing", with the audio and `service playing` lines logged); and controls, the two seeks logged landing on `seekDone`. Pause and resume rest on the tester's words alone: the paste has no pause line, and the backend logs none for the service's `paused` event. Two earlier product runs on the same set support it and are not relied on for the rule. On 2026-11-11 the first, answering the maintainer's request for the debug build of main at `68527cb2` (which carries #1314), pasted no lines and reported "I can confirm that everything worked: picture size/aspect, spinner clears, remaining time counts down, pause/ resume and seek also working." On 2026-11-12 the second, a 16:9 Dolby Vision MKV with one audio track and no subtitles, was "also successful" ("Audio plays, pause, resume and seek works"); its paste has the override line, `uri: exported window placed src=3840x2160 rv=1` and seeks at 31724 and 87765, after the boot line from before this entry, `uri: product binding=Unbound mode=2 handle=false ptype=10 initialized=false major=10 evidence=none`. (c) The drive probe, on an older build (the earlier grader), printed `load=ok playing=yes ... currentTime_advanced=yes ... window=exported ... shape=named end=script pause=unconfirmed ... resume=sent seek=hit seek_delta=-742 ... audio=ok ... pause_basis=reported_positions pause_samples=0 ... paused_event=yes`. Its lines: `drive pause pos=2410`, `ev=paused` at t=4701, no `currentTime` until after `drive resume pos=2410`, then `ev=currentTime value=2729`; `drive seek target=24937`, `ev=seekDone`, the first `currentTime` after it 24195; `drive selectTrack type=audio index=1`, `ev=trackSelected type=audio index=1` (its file had at least two audio tracks; the product runs' had one); `currentTime` in milliseconds about 1 s apart; the tester's answers: pause froze the picture and resume carried on from there ("Yes"), the seek "worked", "Subtitles worked, audio switch also worked" (the subtitles are the firmware's, not our overlay). The tester also reported on that probe run that the video "shows ‘connecting…’ and the buffering icon all the time" and the remaining time "only shows -0:00"; the first product run (2026-11-11) reports the spinner clearing and the remaining time counting down. **Amendment (1) is used.** The run printed `pause=unconfirmed`: with `pause_samples=0` the grader had no position to compare, because the service sends none while paused. The current grader's verdict on the run's recorded numbers stands in for the printed `pause=froze`: `uri_drive_webos_25_c2_run_replays_to_pause_froze_from_the_paused_event_and_resume` (`rust-modules/play-uri/src/uriprobe.rs`) replays them (a pause at 2410, a `paused` event inside the dwell, no samples, 2729 as the first position after the resume) and the current grader answers `pause=froze` on `pause_basis=paused_event_and_resume`; the same test shows that a resume more than 1000 ms from the paused position (3411, 1409) confirms nothing. That verdict is the test's, not a printed one. This older summary prints no `resume_from=` token, so rule (c)'s resume comparison is read from the log: first position after the resume 2729, 319 ms past the paused 2410, inside the bound. Amendment (2) is not used: no control failed. Extra datapoint, not part of the rule: a file with a DTS audio track also played as a direct play on this C2 (the tester says the set uses `dts_restore_plus`), and Diagnostics read `Audio dca -> unknown`: the row's third stage is what we declared to Starfish, and it reads `unknown` for a play on the URI backend (`feed_attempt` in `rust-modules/src/app/diagnostics.rs` on main today; the report names no build), which is expected on a URI play. Not checked: subtitles on the URI path through the product, HLS, audio-track switching through the product (a multi-audio original goes to Standard), end of stream, other 10.x minors, and a build with this table entry on a 10.x set. The 2026-11-14 run adds none of these: one audio track, no subtitles, no `evidence=table` line in its paste, and no end-of-stream line (the backend logs none for it, so a paste could not show one). |
| webOS 26: set on 11.1.0-3701, mechanism read on the 11.2.0 image | LG G4 (issue #1064) | `windowId` in `load` → 4K picture, Dolby Vision and Atmos through the TV's pipeline, our overlays composited over it, correct size and place. The set runs 11.1.0-3701; 11.2.0 is the unpacked image the mechanism above was read from, not the set. |
| Dolby Vision on a URI Load (static reads 2026-11-17, 2026-11-20 and 2026-11-21; no device) | unpacked images 4.x, 5.5.0-19, 5.6.2-21, 6.5.3-47, 7.6.0-1901, 8.3.1-42, 9.2.4-48, 10.3.1, 10.3.2, 11.2 | **The node is presence-only; the demuxer decides Dolby Vision, and MKV gets it from 10.3.** (a) `StarfishMediaAPIs::parseLoadMessage` (5.5.0 `libplayerAPIs` `51d91f5b` ref 0x3f2f6 and `starfish-media-pipeline` `9b122b13` ref 0x3ca9e; 5.6.2 `libplayerAPIs` `6a6f072d`, store 0x3f308) reads `option.externalStreamingInfo.contents.DolbyHdrInfo` only for existence and sets hdrType to DolbyVision (`hdr+4 = 2`). From then on `updateHdrInfo` drops later HdrInfo from the pipeline. It also passes `isDolbyVision` to `ResourceManager::acquireResources` (effect not determined), and from 7.x on (7.6.0 `0969903e` at 0x46e1e-0x46fb0, 8.3.1 `e5ab26e1`, 9.x-11.x; not on 6.5.3) sends `setAppCloseInfo` with `hdrType` DolbyVision. On 8.3.1 the node is presence-only the same way (ref 0x47598; `hdr+4 = 2` at 0x475f4; `updateHdrInfo` 0x4f77c returns early on it). `profileId`/`trackType` are read only by libpf `CustomPipeline::parseOptionStringSpi` (Feed). (b) LG `qtdemux` writes `dolby-vision=TRUE`/`dolby-vision-profile` from `dvcC`/`dvvC` when `platformSupportDolbyVision` is set, on every image read (5.5.0 `libgstisomp4` `a7272ce2` 0xd108; 5.6.2 `48df5a28` FUN_0001d108; 8.3.1 `ddd4ee8a` `configure_stream` 0xdb28); from 5.5.0 on, a profile 7 file gets no DV caps. (c) `matroskademux` has no Dolby Vision code before 10.3. On 5.5.0 (`c68f381f`) and 5.6.2 (`60a0941b`, the same code) `gst_matroska_demux_parse_stream` (0x10548) skips every BlockAdditionMapping id as an unknown TrackEntry child. 6.5.3 (`9a6bd2f7`), 7.6.0 (`12966041`), 8.3.1 (`d07223a1`, `parse_stream` 0x11148: 0x41E4 falls to "Unknown TrackEntry subelement") and 9.2.4 (`07cfe1ee`) read the same way, and a constant scan of 19 builds from W16 to W24 finds no `dvcC`/`dvvC`. 10.3.1/10.3.2 (`54a03ad3`) parses BlockAdditionMapping, matches `dvcC`/`dvvC` as constants (0x12ab2), and writes `dolby-vision`/`dolby-vision-profile` caps (0x1616c/0x16186) when the `dolby-vision-support` smart property is set; profile 7 plays as HDR10. 11.2 carries the same code (scan). (d) On 5.6.2 no userspace element finds Dolby Vision from the HEVC stream: `lxvideodec`/`dvbin` (`6df64857`) never inspect NAL 62/63, h265parse (`25425376`) acts on NAL 62 only when its input caps already carry `dolby-vision`, and `gst_h265_parser_parse_dv_rpu` (`libgstcodecparsers` `696f79c4`) has no caller. The 5.6.2 CX row's Profile 8.6 MKV badge with no node is therefore **not explained by userspace**; the kernel VDEC / `libkadaptor` DOVI backend (driven by V4L2 events from `/dev/video240`) is a lead that cannot be read from the image. The `hdrType` the app's `videoInfo` reported during that play would tell the two apart; it was not recorded. (e) On 5.5.0-19 `dvbin` (rank 291) builds `dvsplitter` (`0cda1717`, IDR 19/20 first) for any URI stream that carries DV caps, with or without the node; on 6.5.3-47 `dvbin` builds no splitter, and neither does 8.3.1's (`lxvideodec` `6d231593`: one `lxvideodec`, and no `libgstdvsplitter` in the image). Not determined: `libcbe.so`, kernel-side RPU detection. Addresses are file VAs (Ghidra's are +0x10000). |
| webOS 3.x | unpacked images | no `libSDL2` at all; out of scope. |

## What the 5+ firmware actually does with the load

Read from `libplayerAPIs`, `libav-proxy`, `libav-connector`, `libav`, `videosink` and
`umediaserver` across 5.5, 5.6, 6.5, 7.6, 9.2, 10.3 and 11.2 (on 8.3.1 only `libplayerAPIs` and
`starfish-media-pipeline` were read):

- **`umediaserver` does not parse `option` itself.** `loadCommand` reads `uri`, `type`,
  `payload`, `option` and `payload.appId`, requires `uri` then `type`, and hands the payload to the
  registered pipeline. Its only `windowId` reader is the separate `setWindowId` command — which is
  why sending it after the load is too late.
- **`libplayerAPIs::parseLoadMessage` parses `option.windowId`** (the `get_optional("option.windowId")`
  parse is identical on 5.5, 5.6, 6.5, 7.6, 8.3, 9.2, 10.3, 11.2; the field lands at `MediaApiPrivate+0x7c`
  on 5.x, `+0x90` on 6.5.3, 7.6.0 and 8.3.1, `+0x94` on 9.2.4 and 10.3.1, and `+0x9c` on 11.2 per the firmware
  report), stores it, and passes it to a virtual call. The firmware report identifies that call on 11.2
  (slot +0x78 of the object at `MediaApiPrivate+0x60`) as the AVProxy builder, which `createAVProxy`
  uses. On 8.3.1 the call is slot +0x78 of `+0x5c`, which that image's `MediaApiPrivate` constructor makes a
  `subtitle::SubtitleTask` (the slot is its `setLayerInfo`, given the window id first), and `createAVProxy`
  reads `+0x90` into its `AVProxy::Builder` itself (the 8.3.1 Results row). On 6.5.3 (slot +0x74 of `+0x5c`),
  7.6.0 (+0x74 of `+0x5c`) and 9.2.4 (+0x78 of `+0x60`) only a virtual call at the same point in the code
  was seen, and the slot was not separately identified there.
- **`option.windowType`** is decoded alongside it: `"InitialWindow"` → 402, `"NoWindow"` → 401,
  `"NoDisplayControl"` → 403, absent or unrecognised → 400. `AVProxy::checkValidation` requires a
  non-empty window id **only when the type is the default 400**, which is what our payload sends.
  `NoWindow` is the "do not display" case, not a fallback.
- **`error 104 "fail to create AVProxy"` is not a window-specific error.** It is emitted whenever
  `createAVProxy()` returns zero: a missing window id (type 400), a failed connector registration,
  or a resource failure. The neighbouring 601 codes are separate resource failures.
- **The binding backend on 5+ is `libav-connector` → `com.webos.service.videooutput`**
  (`register`, `connect`, `setVideoData`, `display/setDisplayWindow`, `display/setDisplayWindowProperty`),
  with `com.webos.service.utp.extinputs/setPipelineWindowId` for external inputs. `libAcbAPI` is
  absent from every 5+ image.
- **The exported window is the compositor's.** `SDL_webOSCreateExportedWindow` /
  `SetExportedWindow` / `DestroyExportedWindow` exist on every 5+ image (`SDL_webOSSetInputRegion`
  from 6.5, `SDL_webOSSetLSHandle` from 7.6) and on none of the 4.x ones; the id is assigned over
  `wl_webos_exported` and handed back to SDL.

### Dolby Vision on the load

A URI Load carries no `DolbyHdrInfo` node. On every image read, `StarfishMediaAPIs::parseLoadMessage`
only checks that the node exists (5.5.0 `libplayerAPIs` sha256 prefix `51d91f5b`, 5.6.2 `6a6f072d`)
and then pins `hdrType` to `DolbyVision`, ignoring the `HdrInfo` the pipeline reports after it; nothing
reads `profileId` or `trackType`. Whether Dolby Vision reaches the picture is the TV's demuxer's call
(a static Ghidra read of the images, sha256 prefixes):

| image | `matroskademux` Dolby Vision | `qtdemux` Dolby Vision |
|---|---|---|
| 4.10.2 | no: no `dvcC`/`dvvC` constant (`libgstmatroska` `57f3f5a5`) | yes (`libgstisomp4` `e2e36fdf`) |
| 5.5.0-19 | no: `parse_stream` (0x10548) skips every `BlockAdditionMapping` id (`c68f381f`) | yes (`a7272ce2`, 0xd108); Profile 7 excluded |
| 5.6.2-21 | no: the same code (`60a0941b`) | yes (`48df5a28`); Profile 7 excluded |
| 6.5.3-47 | no (`9a6bd2f7`, `parse_stream` 0x10fd8) | yes; Profile 7 excluded |
| 7.6.0 | no (`12966041`, 0x1120c) | yes; Profile 7 excluded |
| 8.3.1-42 | no (`d07223a1`, `parse_stream` 0x11148) | yes (`ddd4ee8a`); Profile 7 excluded |
| 9.2.4 | no (`07cfe1ee`, 0x112f0) | yes |
| 10.3.1 / 11.2 | yes: `parse_stream` (0x12930) matches `dvcC`/`dvvC` and sets `dolby-vision`; Profile 7 plays as HDR10 (`54a03ad3`; 11.2 by scan only) | yes |

So the node would only label a stream the demuxer already flags, and where the demuxer does not flag
it the set shows the base layer under a Dolby Vision label (the LG C1 run in the 6.5.3 row above). An
original Standard plays as Dolby Vision is therefore offered to the URI engine only where the TV's
own demuxer carries it (`pipeline_carries_dolby_vision` in `rust-modules/media/src/route/uri_preflight.rs`,
the Matroska half `Caps::dv_matroska` / `matroska_dolby_vision_for` in `rust-modules/os/src/devcaps.rs`):
an MP4, or an MKV from webOS 10.3 (no 10.0-10.2 image has been read, so those count as before it),
and never a Profile 7 that Feed converts. Anything else stays on
Standard (`uri: original declined (the TV's own player would show only this file's Dolby Vision base
layer; ...)`). One loose end: the 5.6.2 CX run's Profile 8.6 MKV showed the Dolby Vision badge with no
node, which nothing above explains; the decline does not depend on it, since that file plays Dolby
Vision on Standard.

## What the 4.x firmware has instead

- **No window concept anywhere in the media path.** `loadCommand` takes `uri`, `type`,
  `payload.option`, `mediaId`, `aliveMode` under a `{}` schema, so an unknown key such as
  `windowId` is accepted and silently ignored. `libpf`'s option parser reads 120 (4.4.2) to 127
  (4.10.0) `option.*` paths and contains no window key at all. `AVProxy` appears nowhere in
  `/usr` or `/etc`, which is why 4.5 could never produce code 104.
- **The picture goes to a SoC plane chosen by a config file.** `/etc/gst/gstcool.conf` selects
  `dvovideosink` (Realtek DVO) on 4.10.0 and `mvsink` (MStar MVOP) on 4.4.2; the sinks position the
  plane themselves.
- **The only app-facing control is ACB** — `libAcb`/`libAcbAPI` → `com.webos.service.acb`, keyed on
  `appId` + `pipelineId` and a **rectangle** (`setDisplayWindow`, `setMediaVideoData`,
  `setWindowZOrder`). No window handle exists.

## The earlier 4.x probe failure was quiet

On webOS 5.4.2 a `load` without a window is refused. In the earlier 4.5 run the payload returned `ok`, the
pipeline fetches the file and decodes it — `videoInfo 1918x802` at 24 fps, audio, a position that
advances — and nothing appears on screen. Nothing errors anywhere.

```
uriprobe: t=0 armed variant=at-load scheme=http kind=direct window=none app=com.beb.plxnative.debug
uriprobe: t=16 register shape=named ok
uriprobe: t=69 call load ok
uriprobe: t=1148 ev=videoInfo width=1918 height=802 frameRate=24
uriprobe: t=1301 ev=playing
```

Two service captures 15 s apart were black in that run. It established an unbound picture for
that probe configuration, not a general 4.x restriction.

The later [controlled ACB experiment](measurements/uri-acb-dass-2026-11-02.md) used a successfully
initialized player-type-0 handle, foreground notification, actual video facts, an explicit
SDR720p24 fixture descriptor and paced full-screen ACB binding. Both `acb-control` and `acb-dass`
displayed moving video in DISPLAY and VIDEO service captures. The control omitted `connectDass`,
so it was not required for that demonstrated sequence. The contribution of the shared setup
changes remains undetermined. Neither arm created an SDL exported window; `window=none` alone
therefore does not determine whether an ACB arm can display its picture.

That sequence is now one shared machine (`rust-modules/play-uri/src/acb_bind.rs`) that the probe and
the production URI backend (`Binding::Acb`, routed only for the Playback engine opt-in) both step. The production
backend never uses the fixture descriptor that the controlled arms fell back to (a developer-probe
exception). It sends the video-data envelope the service itself reported in the ACB schema (a
`sourceInfo` with a string `context` and a decodable `video`) when there is one, and otherwise an
envelope derived from the service's own `videoInfo` event. The 4.5 service never sends the ACB schema:
its `sourceInfo` is a track list, so the controlled arms saw `ev=sourceInfo` and still used the fixture
descriptor, and a production bind that waited only for the schema timed out and fell back to Feed. The
firmware's own conversion (libplayerAPIs builds the envelope Feed binds with from the same members the LS2 `videoInfo` reports)
is measured and mapped in [the envelope measurement](measurements/uri-acb-envelope-2026-11-05.md); the
backend applies that map as pure functions (`acb_envelope.rs`), writes only the firmware's own constants
itself, and builds nothing when a member the firmware always writes is missing, so a run with no usable
`videoInfo` still waits and times out. Whether ACB accepts the derived envelope on the set has not been
measured yet. The ACB plane is released
by `UNLOADED` and a fence on its completion callback
([evidence](measurements/acb-release-fence-2026-11-04.md)), so a later bind in the same process waits
for that fence instead of needing a relaunch; only a failed fence quarantines the plane until exit.
The production backend also reports the first picture (`Observation::Presented`) on this binding,
once the service has played, the firmware has completed the bind's window and PLAYING calls, and the
service's clock then advances (`docs/uri-first-migration.md`); the probe shares the display step but
reports no observation.

## Driving it: what is settled and what is not

The `drive` variant of the probe binds like `load` and then, three seconds after `playing`, pauses,
resumes, seeks 20 s ahead, selects a text and an audio track, and enables subtitles, logging the
position around each call. In the earlier 4.5 `drive` run without the later ACB binding sequence:

```
drive pause pos=2449   call pause ok
drive resume pos=2449  seek target=20333  call seek ok  ev=seekDone
call selectTrack text ok        (accepted, no trackSelected)
call selectTrack audio ok  ev=trackSelected
drive subtitles enable  call setSubtitleEnable ok
summary … pause=froze pause_delta=0 resume=ok seek=hit seek_delta=-968 text=ok audio=ok subs=ok
```

This is a historical summary from the earlier grader. Its cached positions and broad pause
tolerance do not prove a pause. The current probe requires fresh, equal post-Pause positions
covering at least half the dwell, including a report in the Resume observation frame; otherwise
it leaves a stationary-position verdict unconfirmed. Advancing positions report movement.
Method acknowledgements and service state events remain separate observations.

Settled on 5+ by the 5.6.2 runs (Results; the product run and the drive probe) and the 10.x drive
probe: the service sends a `paused` event (then `playing` on resume) and no `currentTime` while
paused; `play` after `pause` resumes from the paused position rather than restarting (2439, then 2641
on 5.6.2; 2410, then 2729 on 10.x); and a `seek` lands on the service's `seekDone`. Still open on 5+:
subtitles on the URI path (the 10.x set rendered them in the firmware, a probe report, not a product
run), native track identity and ordering (the 5.6.2 service lists audio tracks in a different order
from the container), HLS, majors other than 5 and 10, and what the seek bar can be fed — the service reports
`currentTime` but no duration, so a duration has to come from `sourceInfo` or from Plex.

The production backend's first Play waits behind the startup selection of the viewer's audio track
(the common startup owner sequences track, seek, then Play, each confirmed). On the 4.5 service that
selection is already made at load: `audioInfo` names it (`{"audioInfo":{"track":0,...}}`, before
`loadCompleted`), the `selectTrack` for it is acknowledged, and no `trackSelected` follows (none in the
2026-11-05 `ls-monitor` capture), so a backend that waited for the event never played and its control
timed out 15 s later. The backend therefore settles a selection of the track `audioInfo` (or a
`trackSelected`) last reported without a native call (`uri: audio index=N is already selected
(audioInfo); no selectTrack sent`), and an `audioInfo` that names a pending select's track after its
acknowledgement settles it like `trackSelected` would. A `selectTrack` the backend does send is logged
first, `uri: selectTrack sent kind=audio index=N stream=SID` (the native index and the Plex stream id), and a
`trackSelected` the service names is traced with its `type` and `index`. The dev route maps the selected
stream to its 0-based position among the Part's audio streams in container order. Measured only with a
one-track source: that `audioInfo.track` and the `selectTrack` index are one numbering, that this position is
that index, and whether a select of a different track before `play` is confirmed at all, are not. A control that is never confirmed is
refused when its deadline passes and logs `uri: control ... unconfirmed after N ms; refused
(TimedOut)`.

A start that carries a resume position sends `seek` before the first `play` (track, seek, then Play).
The service answers that call with the plain method reply, which has no `seekDone` in it, and reports
`{"seekDone":{"mediaId":...}}` on the subscription some 110 ms later (the `bound-controls` bus
capture; the TV's `..._bound_collapse` case waited 15 s for a `seekDone` in the reply and fell back
to Feed). The backend lands the Seek on that event (`uri: control Seek(..) landed on the service's
seekDone (its reply acknowledged first: yes|no)`): in whatever order the reply came, only for the
loaded media's id, only when exactly one Seek is pending, and never after a refusal. Not determined:
whether a `seek` sent before `play` is answered at all (the capture's came after `play`); the
landing line says whether the reply had been acknowledged.

On the exported window (webOS 5+) the production backend places the window as the probe and Feed do:
it creates it at the start and places it with the 1920x1080 canvas as the source, then, when the
service's `videoInfo` reports the coded size, again with that size as the source; the destination is
always the whole canvas (`uri: exported window placed src=WxH rv=N`). Production selects that binding
only for a firmware major in the evidence table described below, which admits majors 5 to 10 today. A debug build
composes it for one boot when the file `plxnative-uriexported` exists in the install's runtime root
(content ignored). Unlike `plxnative-uriprobe`, which is read at each Play, this
trigger is read once, at boot, so it can be armed only by writing the file BEFORE the app launches: on
plain `/tmp`, or at the jail's persistent path (routes 1 and 3 under "Arming the trigger" in the tester
run below). Route 2, through `/proc/<pid>/root` while the app runs, cannot arm it: that write comes after
the boot read and the override stays off. The line that confirms it is
`uri: product binding=Exported { canvas: (1920, 1080) } mode=2 evidence=override`, which follows the
production `uri: product binding=Unbound ..` line on a major the table has not admitted (on a 5.x to
10.x set the production line is already `binding=Exported { canvas: (1920, 1080) } ... major=5
evidence=table` (or `major=6`, `7`, `8`, `9` or `10`), and the override line, when armed, still follows it). A log without it proves nothing about the product
backend of a major the table has not admitted. The viewer still has to opt in to the URI engine, and the override does nothing on a display
whose mode is not the exported window. Whether the service sends `videoInfo` before the first frame is
not determined, so the first frames may be placed from the canvas source rectangle.

Production selects the exported window per firmware major, from the table `EXPORTED_STRATA`
(`rust-modules/play-uri/src/backend.rs`), which admits majors 5 to 10 today (the Results
rows above: 5.6.2; 6.5.3; 7.6.0; 8.3.1; 9.2.4 / 10.3.1): a 5.x set's boot line reads `major=5 evidence=table`,
a 10.x set's `major=10 evidence=table`, and a 6.x, 7.x, 8.x or 9.x set's the same with its own major. Majors
5 and 10 were admitted on tester runs; 6, 7, 8 and 9 were admitted as bracketed, with no tester run.
Every other webOS 5+ major plays Standard (its boot line reads `major=<n> evidence=none`; a 4.x set
reads `none` too and keeps its ACB path). An entry is a
webOS firmware major as the set reports it, so major 5 covers every 5.x release: the product run was
on 5.6.2, the earlier probe-only run on 5.4.2, and no other 5.x minor has a run. Major 10 covers every
10.x release the same way: the product runs (the rule's is the 2026-11-14 one, on a 2.39:1 original) and
the probe were on one LG C2 on 10.3.1 (33.31.68), and no other 10.x minor has a run. Majors 6, 7, 8 and 9 each cover every release of that major the same
way, though one image per major is all that has been read (6.5.3, 7.6.0, 8.3.1, 9.2.4) and none has a
rule (b) product run (the 6.x C1's Dolby Vision plays, in the 6.5.3 row, are not one).
An entry needs three things in this file's Results table, each with the tester log's decisive
lines (one exception follows the list). A release is never admitted on the probe's evidence alone: a run of the product backend is
required.

- (a) the major's own firmware image reads `option.windowId` at load (decompile);
- (b) picture: a tester's PRODUCT-path run on a debug build (the boot log shows `evidence=override`)
  shows a moving picture with sound, placed and proportioned right, supported by the probe's summary
  tokens `load=ok`, `playing=yes`, `currentTime_advanced=yes` and `window=exported`;
- (c) controls: the probe's `drive` tokens (`pause=froze` on either `pause_basis`, `resume=sent` with
  `resume_from=` at or after the paused position minus 1000 ms, `seek=hit`, and `audio=ok` or an
  `audio_at_load=`) AND the product run's pause, seek and audio doing what the viewer asked. Two
  amendments, each named in the row that uses it. (1) For a run made before the current grader, the
  current grader's verdict on that run's recorded numbers, replayed in a unit test, stands in for the
  printed `pause=froze`: the row names the test and says the run itself printed `pause=unconfirmed`. The
  5.6.2 row does not use it (its probe run printed `pause=froze`); the 10.x row does, with
  `uri_drive_webos_25_c2_run_replays_to_pause_froze_from_the_paused_event_and_resume`. (2) A control the product run showed
  failing is acceptable only when the route now sends that case to Standard (the 5.6.2 audio-track
  switch, routed by #1314 and since observed on the set as `uri: original declined (several audio tracks
  on the exported window)`); the row names it, and the admission covers only the route as it now stands.

One exception, the owner's decision of 2026-11-13, "nearest admitted major on each side": a major
with (a), its own image showing the same `windowId` load path, but no tester run is admitted when the
nearest admitted major below it and the nearest admitted major above it were both admitted on tester
evidence, and its Results row says "bracketed, no tester run". A major admitted only as bracketed was
not admitted on tester evidence, so it is skipped when looking for a side and never extends a
bracket: 6, 7, 8 and 9 each stand between 5 and 10, never between one another. A major with no image
to read has no (a) and is not admitted. Nothing else is inferred from a neighbouring major. Each entry names its Results row, and a webOS 5+ major without
an entry plays Standard, with no handoff.

## Reproducing it

1. Deploy a debug build carrying the probe (devtriggers; no `RELEASE=1` package contains it).
2. Arm a variant in the install's runtime root:
   `echo load > /tmp/com.beb.plxnative.debug/plxnative-uriprobe` (empty, `load`, `late`, `none`, or
   `drive`). The separate controlled `acb-control` and `acb-dass` arms need no `ptype` trigger since the
   URI engine has its own ACB handle (player type 0), which the offer of an Experimental play's source (or the probe's gate) creates on
   demand (the boot creates only Feed's, player type 10: `acb handle=feed ptype=10 ..`; the log names
   the URI engine's `acb handle=uri ptype=0 create=.. initialized=true` when the probe or an
   Experimental play first needs it, and `uri: product binding=Acb .. handle=deferred ..` at
   composition), but the harness cases keep arming `ptype=0` as the
   2026-11-02 measurements did (it now sets Feed's handle type, which those arms do not bind), and the fixture
   constraints in the measurement linked above. The 2026-11-02 pair each ran in a fresh process; a second
   arm in the same process binds only after the previous release's fence is proven. A `plex.direct` URL needs
   working name resolution on the set; a plain `http://` URL to
   a LAN server does not.

   To test the TV's own adaptive controller (R-ABR-2), add native bit-rate rates after the
   variant, space separated and in any order: `bpsstart=N` seeds it, `bpsmax=N` caps it and
   `bpsmin=N` floors it, each in bit/s from 1 to 1000000000 and given at most once, for example
   `echo 'load bpsmax=720000' > /tmp/com.beb.plxnative.debug/plxnative-uriprobe`. They become
   `"adaptiveStreaming":{"bps":{"start":N,"maximum":N,"minimum":N}}` inside the `load` option
   (only the rates given) and the `armed` line ends with the same tokens (` bpsmax=720000`). A bare
   `bpsmax=720000` arms the default variant. Any other token, a repeated or non-numeric rate, or a
   rate outside the range arms nothing and the play is normal; the event log says so. Read
   `streamingInfo` (`instantBitrate`, `totalBitrate`), `customizedStreamingInfo` (`numOfRates`,
   `currentIndexOfRate`), `bufferRange` (`remainingTime`, `percent`) and `videoInfo`
   (`currentBitrate`, `contentBitrate`) from the same log: only numbers are printed, never a
   host. Whether the firmware honours the rates, and what those fields mean on the 4.5 set, is
   what the run is for. `tests/run.py --filter pipe_abr_uri_ladder` drives it against a ladder
   master (`tests/README.md`).
3. Play a title the route chooses to direct-play (`kind=direct` on the `armed` line), watch, press
   Back.
4. Read `grep 'uriprobe:' /tmp/com.beb.plxnative.debug/plxnative-events.log`: `window=`,
   `playing`, `currentTime`, and the closing `summary` are the fields that matter.
5. Capture with the service screenshot — the app's own stream and any GL readback cannot see the
   hardware video plane, so a black rectangle there proves nothing either way.
6. Remove the trigger file, or the next Play probes again.

On firmware where the app's `/tmp` is jailed separately from a root shell's (reported on webOS 26),
arm the trigger through the app's own view:
`echo drive > /proc/$(pidof plxnative)/root/tmp/com.beb.plxnative.debug/plxnative-uriprobe`.

## webOS 5+ tester run

One run of the `drive` probe on a 5+ set answers whether the TV's pipeline plays a stream into the exported
window and whether its controls (pause, resume, seek, tracks) answer, in one `summary` line, without reading
the `ev=` lines. It needs a developer build (`devtriggers`; no `RELEASE=1` package carries the probe). The probe
plays one stream through `com.webos.media` with no Engine and reports nothing to Plex. Play from your own
server, with a title that direct-plays (`kind=direct` on the `armed` line) and is at least a minute long (the
script seeks 20 s ahead and runs about 18 s from `playing`).

### Arming the trigger

The trigger is a file named `plxnative-uriprobe` in the app's runtime root, `/tmp/com.beb.plxnative.debug/` for
the debug install (`<app id>` below). Most dev triggers are read once at boot; this one is read **at each Play**,
so it can be armed while the app runs. Pick the route that matches the set:

1. **Plain `/tmp`** (the app's `/tmp` is the one a root shell sees):
   `echo drive > /tmp/com.beb.plxnative.debug/plxnative-uriprobe`
2. **Jailed `/tmp`, app running** (reported on webOS 26): through the running process's own root,
   `echo drive > /proc/$(pidof plxnative)/root/tmp/com.beb.plxnative.debug/plxnative-uriprobe`.
   This needs the process, so launch the app first.
3. **The jail's persistent path.** `find /var/palm/jail -type d -path '*/tmp/com.beb.plxnative.debug' 2>/dev/null`
   prints the directory when the set keeps one, `/var/palm/jail/<jail>/tmp/com.beb.plxnative.debug/`; write
   `plxnative-uriprobe` there. Not established: whether that directory exists before the app's first launch, and
   whether it survives the app exiting.

`drive` binds the window exactly as `load` does and then runs the transport script, so one `drive` run also
answers the `load` question. Then play the title and let the script finish (about 18 s after `playing`) before
pressing Back. A run is only evidence if the log has the line
`uriprobe: t=0 armed variant=drive ... window=exported ...`; with no `armed` line the trigger was not read and
the run shows nothing, so say so in the report. Remove the trigger file afterwards, or the next Play probes
again.

### Copying the log

The probe writes to `plxnative-events.log` in the same runtime root. The app truncates that file at each
launch, so copy it before relaunching, and on a jailed set copy it **while the app is still running** (the
`/proc/<pid>/root` route disappears with the process):

```
grep 'uriprobe:' /proc/$(pidof plxnative)/root/tmp/com.beb.plxnative.debug/plxnative-events.log > /tmp/uriprobe-run.txt
```

(on an unjailed set, or through the jail's persistent path, read `plxnative-events.log` from that directory
instead). The `uriprobe:` lines carry no URL, host or token; the rest of the event log is scrubbed but is
not needed. Post the lines and say what the screen showed: the probe cannot see the video plane, so picture
yes/no, sound yes/no, where the picture sat and whether its aspect was right are the tester's to report.

### What the lines say

The `armed` line names the set:

```
uriprobe: t=0 armed variant=drive scheme=<http|https> kind=<direct|hls> window=<exported|none> app=<app id> fw=<release|-> build=<build|->
```

`fw=` and `build=` are the release (`5.4.2`) and the exact firmware build (`5.4.2-47`) the set reported at boot,
`-` when it reported none. They are printed only when they are plain version text (letters, digits and `. - _ +`,
up to 32 characters).

The terminal line, which follows the last event, is (a `load` run prints the first group only):

```
uriprobe: t=<ms> summary load=<ok|fail(<phase>)> playing=<yes|no> t_load=<ms|-> t_playing=<ms|-> currentTime_advanced=<yes|no> videoInfo=<seen|none> audioInfo=<seen|none> window=<exported|none> setWindowId=<n/a|ok|refused|noreply> variant=<variant> scheme=<http|https> shape=<named|anon> end=<end> audio_at_load=<n|none> clock_period_ms=<ms|->
    pause=<..> pause_delta=<ms|-> resume=<..> seek=<..> seek_delta=<ms|-> text=<..> audio=<..> subs=<..> eos=<yes|no> pause_basis=<reported_positions|paused_event_and_resume> pause_samples=<n> pause_span_ms=<ms|-> pause_end_fresh=<yes|no> paused_event=<yes|no> seekdone=<yes|no> resume_from=<ms|-> text_ev=<yes|no>
```

(one line; wrapped here). The tokens a run is read by:

| token | value | reads as |
|---|---|---|
| `load=`, `playing=`, `currentTime_advanced=`, `window=` | `ok`, `yes`, `yes`, `exported` | the service loaded the stream into our exported window, said it plays, and its clock advanced. With the tester's report of a moving picture and sound, correctly placed and proportioned, this is a pass for the picture. `load=fail(<phase>)`, `end=refused:<phase>` and `end=error:<code>` name where it stopped (`error:104` is the AVProxy failure above) |
| `shape=` | `named`, `anon` | the registration identity the service accepted: the named client first, then the anonymous one. A set that refuses both ends `end=refused:register` |
| `pause=` | `froze` | fresh, equal positions span at least half the pause dwell and reach the Resume frame, or the service emits `paused` during the dwell, reports no position until Resume, and the first pre-seek position is within 1 s of the position at Pause. `pause_basis` names the evidence |
| | `moved` | Pause was accepted but the position kept running |
| | `unconfirmed` | too few or too stale reports to say: rerun, this is not a pass |
| | `refused`, `none` | the call was refused, or the script never reached it |
| `resume=` | `sent` | the Resume `play` call was sent and not refused; a reply is not required. It is the best this probe prints (nothing but the position shows whether playback continued; read `resume_from=`); `refused` if the service refused it |
| `resume_from=` | `<ms>` | the first `currentTime` the service reported after the Resume call was sent. Compare it with `pos=` on the `drive pause pos=<ms>` line above it: a value at or after that position minus 1000 ms is a resume; a value near 0 is a restart from the beginning (the 4.5 set's first position after resume was 333 ms). A report already in flight when Resume was sent can still read the paused position, so an equal value next to a picture that restarted is worth a rerun |
| | `-` | no position arrived after the call, or the script did not reach it |
| `seek=` | `hit` | the first position after Seek was within 5 s of the target (`seek_delta=` is how far off) |
| | `miss`, `refused` | it was not, or the call was refused |
| | `sent`, `none` | `sent`: the service answered the call ok but no position arrived after it. `none`: the call was not answered and no position arrived after it, or the script never reached it |
| `seekdone=` | `yes`, `no` | a `seekDone` event arrived on the subscription after the Seek call was sent. A `seekDone` from before the call does not count. `seek=hit` with `seekdone=no` means the position moved but the service never said the seek finished |
| `audio_at_load=` | `<n>` | an `audioInfo` that named track `n` arrived before (or in the same message as) `loadCompleted`: the service had already selected an audio track at load. `none`: no `audioInfo` named a track by then |
| `audio=` | `ok`, `sent`, `refused`, `none` | the `selectTrack` for audio index 1 (the script's second audio track). `ok` needs the call acknowledged and a `trackSelected` or an `audioInfo` naming index 1 (in either order); `sent` is a call the service answered without refusing it, with no such event; a one-track title may refuse it |
| `text_ev=` | `yes`, `no` | a `trackSelected` event of type `text` arrived after the script's text `selectTrack` was sent. A bare `trackSelected`, an audio one, or one from before the call is `no` |
| `text=`, `subs=` | as `audio=` | the text `selectTrack` (index 0) and `setSubtitleEnable` calls; recorded, they say nothing about rendering |
| `paused_event=` | `yes`, `no` | a `paused` event arrived during the scripted pause dwell |
| `clock_period_ms=` | `<ms>`, `-` | the median gap between successive `currentTime` reports over the whole run (for an even count, the smaller of the two middle gaps), on the app's frame clock, so one frame (about 16 ms at 60 fps) is its resolution. `-` for fewer than two reports. It says how closely a subtitle clock could follow the service's |
| `eos=` | `yes`, `no` | an `endOfStream` event arrived |
| `end=` | `script` | the script ran to its last step and the probe stopped itself. A complete `drive` run ends here |
| | `back` | Back was pressed before the script finished: the later calls may not have run, so rerun and wait |
| | `eos`, `error:<code>`, `timeout:<phase>`, `refused:<phase>` | the stream ended, the service errored, a wait ran out, or a call was refused: not a pass for the controls |

Everything else on the line is the raw material for the rest: `t_load=` and `t_playing=` are the milliseconds
from `armed` to `loadCompleted` and to `playing`, `videoInfo=` and `audioInfo=` say whether the service reported
the stream at all, and `pause_*` give the position samples behind the `pause=` grade.

### Why a play stayed in Standard

The `uriprobe:` lines come from the probe. Separately, a viewer whose **Settings > Video & playback > Playback
engine** is Experimental gets one line in the same event log for each play the URI engine did not take (the play
went to Standard):

```
uri: original declined (<reason>)
```

It is written once per resolve, after the plan is chosen, and only for Experimental: Standard says nothing. It
names no title, host, URL or token. The reason is the first check that said no, in the order of this table, and
the last column is what the tester can change. A play the URI engine took has no such line while it plays; one
that fails over from the URI engine to Standard writes the `already tried` line when that handoff resolves.

| `<reason>` | meaning | what to change |
|---|---|---|
| `no URI engine in this build or boot` | the build has no URI engine, or the boot refused to install the URI driver (`uri: product driver installation refused`) | none |
| `the URI original was already tried or is not offered for this play` | this play already tried a URI original (a play that failed over from the URI engine logs this on its handoff to Standard), or the route refused or excluded it | none |
| `the picked audio or subtitle track is not in this file` | the viewer's track pick names a stream the Part does not list | pick a listed track |
| `playback speed is not 1x` | the play runs at another speed | play at 1x |
| `the quality ceiling or Direct Play setting rules out the original; set quality to Original and Direct Play to Auto` | a fixed quality rung the file exceeds (or has no measured rate for), or Direct Play is Disabled | Quality Original, Direct Play Auto |
| `the link is a Plex relay` | the server is reached through a Plex relay, which never carries an original | none: use a direct or LAN link |
| `quality Auto on a link that is not local; set quality to Original` | Auto streams adaptively unless the link is known to be local, and the app reads this one as remote (or has not measured it) | Quality Original |
| `an audio enhancement is on` | Boost Dialog or Normalize Loudness is on, so the server re-encodes the audio | turn it off |
| `a subtitle is burned in` | a subtitle is burned into the picture; any file with a subtitle stream stays in Standard today | a file without subtitle streams |
| `the selected subtitle is an external file` | the selected subtitle is a sidecar file the app draws; any file with a subtitle stream stays in Standard today | a file without subtitle streams |
| `the file has subtitle streams and none is selected; subtitle streams keep a play in Standard today` | the TV's pipeline cannot switch the file's default subtitle off, and selecting one does not help (it ends as `subtitle streams keep a play in Standard today; try a file without subtitle streams`) | a file without subtitle streams |
| `the file has Dolby Vision and the Dolby Vision setting is Off` | Dolby Vision is Off in the profile | set Dolby Vision to Auto |
| `the TV's own player would show only this file's Dolby Vision base layer; Standard plays it as Dolby Vision` | a Profile 7 that Feed converts, or a Dolby Vision MKV on a set whose Matroska demuxer reads no Dolby Vision (before webOS 10.3; "Dolby Vision on the load" above) | none: the file plays as Dolby Vision in Standard |
| `a trailer preview` | previews are not carried | none |
| `no duration or URL for the file` | the Part reports no duration | none |
| `Playback engine is not Experimental` | the profile is on Standard | none (a Standard viewer gets no line) |
| `Standard would not direct-play the video`, `... the Dolby Vision`, `... the audio`, `... the container` | the file is one Standard itself would convert (a codec or size the set cannot show, a Dolby Vision layering the set refuses, an audio track the set's own decoder cannot play, a container Standard has the server remux), and the TV's pipeline would play it wrongly or silently | none: the file plays in Standard |
| `several audio tracks on the exported window` | the exported window's service can order tracks differently from Plex | a single-audio file |
| `the certified source did not match this play` | the qualifier's certificate was for another selection, track map or Dolby Vision record, or had expired | report it with the line |
| `subtitle streams keep a play in Standard today; try a file without subtitle streams` | a subtitle is selected and the certificate does not carry it: no qualifier certifies a subtitle render yet, so every selected embedded subtitle ends here (by design, not a bug) | a file without subtitle streams |
| `no exported-window evidence for this firmware` | the display path has no evidence on this firmware (no exported window with a canvas, no ACB plane) | none: report the `uri: product binding=... major=...` boot line and the `devcaps: firmware=... build=...` line |
| `the qualifier did not certify the file` | a developer qualifier declined it | none |
| `the ACB handle for the URI engine could not be created` | on a webOS 4.x ACB display the offer makes the URI engine's own handle (player type 0) and its create or initialize failed; the failure is recorded, so every later offer in this run declines at once | none: the play goes to Standard; report it with the `acb handle=uri` line |

The reasons are local diagnostics: they route nothing, and nothing about them is sent to telemetry.

## Open questions

- Whether webOS 5+ can be driven fully as a player: pause, resume and seek are settled on 5.6.2 and
  on the 10.x probe (Results). Open: where a duration comes from, native track identity and ordering,
  end-of-stream (no run has reached it), and HLS and majors other than 5 and 10 on the product path.
- Whether a descriptor (PGS) or text track renders into the exported window, or whether subtitles
  must stay on our own overlay path.
- Which formats the TV's pipeline plays that our buffer-feed path refuses, on stock firmware.
- On 4.x, whether an **in-process** `uri` load — `StarfishMediaAPIs::Load` with a URL instead of
  fed access units — would render through ACB. Nothing has tried it; the app always demuxes
  itself. This remains a separate question from the demonstrated out-of-process ACB fixture.
