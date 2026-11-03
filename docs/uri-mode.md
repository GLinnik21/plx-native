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
| webOS 5.4.2 | LG 65UN80006LA (issue #1064) | `windowId` in `load` → picture, moving, with sound. `late` (`setWindowId` after `loadCompleted`) and `none` → `error code=104 "fail to create AVProxy"`. Both a `plex.direct` HTTPS direct play and an HLS transcode played. |
| webOS 9.2.4 / 10.3.1 (33.31.68) | unpacked images | same mechanism as 5.5 and later (below); not yet confirmed on a device. |
| webOS 26 (11.2.0) | LG G4 (issue #1064) | `windowId` in `load` → 4K picture, Dolby Vision and Atmos through the TV's pipeline, our overlays composited over it, correct size and place. |
| webOS 3.x | unpacked images | no `libSDL2` at all; out of scope. |

## What the 5+ firmware actually does with the load

Read from `libplayerAPIs`, `libav-proxy`, `libav-connector`, `libav`, `videosink` and
`umediaserver` across 5.5, 5.6, 6.5, 7.6, 9.2, 10.3 and 11.2:

- **`umediaserver` does not parse `option` itself.** `loadCommand` reads `uri`, `type`,
  `payload`, `option` and `payload.appId`, requires `uri` then `type`, and hands the payload to the
  registered pipeline. Its only `windowId` reader is the separate `setWindowId` command — which is
  why sending it after the load is too late.
- **`libplayerAPIs::parseLoadMessage` parses `option.windowId`** (identical on 5.5, 5.6, 6.5, 7.6,
  9.2, 10.3, 11.2; the field lands at `MediaApiPrivate+0x7c` on 5.x and `+0x90` on 6.5+), stores it,
  and `createAVProxy` hands it to `AVProxy::Builder`.
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

Open on 5+: whether a `paused` event exists at all, whether `play` after `pause` resumes or
restarts (the first position after resume dropped to 333 ms here), whether text `selectTrack`
produces `trackSelected` and a rendered subtitle, and what the seek bar can be fed — the service
reports `currentTime` but no duration, so a duration has to come from `sourceInfo` or from Plex.

## Reproducing it

1. Deploy a debug build carrying the probe (devtriggers; no `RELEASE=1` package contains it).
2. Arm a variant in the install's runtime root:
   `echo load > /tmp/com.beb.plxnative.debug/plxnative-uriprobe` (empty, `load`, `late`, `none`, or
   `drive`). The separate controlled `acb-control` and `acb-dass` arms require a fresh process,
   `ptype=0` and the fixture constraints in the measurement linked above. A `plex.direct` URL needs
   working name resolution on the set; a plain `http://` URL to
   a LAN server does not.
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

## Open questions

- Whether webOS 5+ can be driven as a player rather than only loaded: pause/resume semantics, seek
  units and granularity, track selection, end-of-stream, and where a duration comes from.
- Whether a descriptor (PGS) or text track renders into the exported window, or whether subtitles
  must stay on our own overlay path.
- Which formats the TV's pipeline plays that our buffer-feed path refuses, on stock firmware.
- On 4.x, whether an **in-process** `uri` load — `StarfishMediaAPIs::Load` with a URL instead of
  fed access units — would render through ACB. Nothing has tried it; the app always demuxes
  itself. This remains a separate question from the demonstrated out-of-process ACB fixture.
