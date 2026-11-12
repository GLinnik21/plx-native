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
| webOS 9.2.4 / 10.3.1 (33.31.68) | unpacked images | same mechanism as 5.5 and later (below); not yet confirmed on a device. |
| webOS 26: set on 11.1.0-3701, mechanism read on the 11.2.0 image | LG G4 (issue #1064) | `windowId` in `load` → 4K picture, Dolby Vision and Atmos through the TV's pipeline, our overlays composited over it, correct size and place. The set runs 11.1.0-3701; 11.2.0 is the unpacked image the mechanism above was read from, not the set. |
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

Open on 5+: whether a `paused` event exists at all, whether `play` after `pause` resumes or
restarts (the first position after resume dropped to 333 ms here), whether text `selectTrack`
produces `trackSelected` and a rendered subtitle, and what the seek bar can be fed — the service
reports `currentTime` but no duration, so a duration has to come from `sourceInfo` or from Plex.

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
only for a firmware major in the evidence table described below, which is empty today. A debug build
composes it for one boot when the file `plxnative-uriexported` exists in the install's runtime root
(content ignored). Unlike `plxnative-uriprobe`, which is read at each Play, this
trigger is read once, at boot, so it can be armed only by writing the file BEFORE the app launches: on
plain `/tmp`, or at the jail's persistent path (routes 1 and 3 under "Arming the trigger" in the tester
run below). Route 2, through `/proc/<pid>/root` while the app runs, cannot arm it: that write comes after
the boot read and the override stays off. The line that confirms it is
`uri: product binding=Exported { canvas: (1920, 1080) } mode=2 evidence=override`, which follows the
production `uri: product binding=Unbound ..` line. A log without it proves nothing about the product
backend. The viewer still has to opt in to the URI engine, and the override does nothing on a display
whose mode is not the exported window. Whether the service sends `videoInfo` before the first frame is
not determined, so the first frames may be placed from the canvas source rectangle.

Production selects the exported window per firmware major, from the table `EXPORTED_STRATA`
(`rust-modules/play-uri/src/backend.rs`), which is empty today: no release is admitted, so every
webOS 5+ release plays Standard (its boot line reads `major=<n> evidence=none`; a major the table
admits reads `evidence=table`; a 4.x set reads `none` too and keeps its ACB path). An entry is a
webOS firmware major as the set reports it, and needs three things in this file's Results table, each
with the tester log's decisive lines. A release is never admitted on the probe's evidence alone: a run
of the product backend is required.

- (a) the major's own firmware image reads `option.windowId` at load (decompile);
- (b) picture: a tester's PRODUCT-path run on a debug build (the boot log shows `evidence=override`)
  shows a moving picture with sound, placed and proportioned right, supported by the probe's summary
  tokens `load=ok`, `playing=yes`, `currentTime_advanced=yes` and `window=exported`;
- (c) controls: the probe's `drive` tokens (`pause=froze`, `resume=sent` with `resume_from=` at or
  after the paused position minus 1000 ms, `seek=hit`, and `audio=ok` or an `audio_at_load=`) AND the
  product run's pause, seek and audio doing what the viewer asked.

One exception: a major with the image evidence but no tester run is admitted when both neighbouring
majors are admitted on tester evidence, and its row then says "bracketed, no tester run". Nothing else
is inferred from a neighbouring major. Each entry names its Results row, and a webOS 5+ release without
an entry plays Standard, with no handoff.

## Reproducing it

1. Deploy a debug build carrying the probe (devtriggers; no `RELEASE=1` package contains it).
2. Arm a variant in the install's runtime root:
   `echo load > /tmp/com.beb.plxnative.debug/plxnative-uriprobe` (empty, `load`, `late`, `none`, or
   `drive`). The separate controlled `acb-control` and `acb-dass` arms need no `ptype` trigger since the
   boot gave the URI engine its own ACB handle (player type 0, created beside Feed's player type 10;
   the boot log names both: `acb handle=uri ptype=0 create=.. initialized=true`, and `uri: product
   binding=Acb ..` at composition), but the harness cases keep arming `ptype=0` as the
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
| `a trailer preview` | previews are not carried | none |
| `no duration or URL for the file` | the Part reports no duration | none |
| `Playback engine is not Experimental` | the profile is on Standard | none (a Standard viewer gets no line) |
| `Standard would not direct-play the video`, `... the Dolby Vision`, `... the audio`, `... the container` | the file is one Standard itself would convert (a codec or size the set cannot show, a Dolby Vision layering the set refuses, an audio track the set's own decoder cannot play, a container Standard has the server remux), and the TV's pipeline would play it wrongly or silently | none: the file plays in Standard |
| `several audio tracks on the exported window` | the exported window's service can order tracks differently from Plex | a single-audio file |
| `the certified source did not match this play` | the qualifier's certificate was for another selection, track map or Dolby Vision record, or had expired | report it with the line |
| `subtitle streams keep a play in Standard today; try a file without subtitle streams` | a subtitle is selected and the certificate does not carry it: no qualifier certifies a subtitle render yet, so every selected embedded subtitle ends here (by design, not a bug) | a file without subtitle streams |
| `no exported-window evidence for this firmware` | the display path has no evidence on this firmware (no exported window with a canvas, no ACB plane) | none: report the `uri: product binding=... major=...` boot line and the `devcaps: firmware=... build=...` line |
| `the qualifier did not certify the file` | a developer qualifier declined it | none |

The reasons are local diagnostics: they route nothing, and nothing about them is sent to telemetry.

## Open questions

- Whether webOS 5+ can be driven as a player rather than only loaded: pause/resume semantics, seek
  units and granularity, track selection, end-of-stream, and where a duration comes from.
- Whether a descriptor (PGS) or text track renders into the exported window, or whether subtitles
  must stay on our own overlay path.
- Which formats the TV's pipeline plays that our buffer-feed path refuses, on stock firmware.
- On 4.x, whether an **in-process** `uri` load — `StarfishMediaAPIs::Load` with a URL instead of
  fed access units — would render through ACB. Nothing has tried it; the app always demuxes
  itself. This remains a separate question from the demonstrated out-of-process ACB fixture.
