# URI mode (`com.webos.media/load`): what each firmware has answered

URI mode hands the stream URL to the platform's media service and lets the television's own
pipeline fetch and play it, instead of the app feeding buffers from its own process. It only helps
if that pipeline renders into a window **this** process exports, and that binding is
`option.windowId` in the `load` payload. `docs/buffer-feed-plan.md` is why we went the
buffer-feed route on webOS 4.5; this page is the evidence on whether URI mode is reachable at
all on a given firmware.

Evidence comes from a probe, not from this repository's own playback path: the `plxnative-uriprobe`
developer trigger (devtriggers builds, so no `RELEASE=1` package carries it) forks one Play into a
`load` through `com.webos.media` and logs what the service said. `load`, `late` (window id via
`setWindowId` after `loadCompleted`) and `none` (no window at all) are the three variants. Probe
plays report nothing to Plex.

## Results

| firmware | set | variant | window | `load` | picture |
|---|---|---|---|---|---|
| webOS 4.5 | this project's dev set (2026-11-02) | `load` | **none** | ok | **black** (two service captures, 15 s apart) |
| webOS 5.4.2 | LG 65UN80006LA (issue #1064) | `load` | exported | ok | yes, moves, with sound |
| webOS 5.4.2 | same | `late`, `none` | — | `error 104 "fail to create AVProxy"` | no |
| webOS 22–26 | — | — | — | not measured | — |

webOS 5.4.2 played both a `plex.direct` HTTPS direct play and an HLS transcode, reported
`videoInfo`/`audioInfo` and a climbing `currentTime`, and accepted the named client registration
from a sideloaded debug app. That is the "picture moves" case the issue was opened for.

## Why 4.5 has no window to give

`src/starfish.c` resolves the video plane once, in `vp_mode()`: an open `libAcbAPI` with its entry
points wins and the mode is `VP_ACB`; only when that fails are the SDL exported-window symbols
(`SDL_webOSCreateExportedWindow` / `SDL_webOSSetExportedWindow`, the webOS 5+ facility) considered,
giving `VP_EXPORTED`. `vp_create_window()` exists on the `VP_EXPORTED` path alone.

On 4.5 the sink is `VP_ACB`, so the probe logs `window=none` and hands `load` no window id — not
because the probe failed to find one, but because this firmware offers no route that produces one.
The only binding 4.5 has is ACB's `tv.display/setMediaVideoData`, which is the documented
`ERROR_06` ceiling. URI mode therefore cannot show video on 4.5, and no ordering of
`load`/`setWindowId` changes that.

The 4.5 log of that run:

```
uriprobe: t=0 armed variant=at-load scheme=http kind=direct window=none app=com.beb.plxnative.debug
uriprobe: t=16 register shape=named ok
uriprobe: t=69 call load ok
uriprobe: t=1148 ev=videoInfo width=1918 height=802 frameRate=24
uriprobe: t=1301 ev=playing
uriprobe: t=1817 ev=currentTime value=75 (first report)
uriprobe: summary load=ok playing=yes ... window=none setWindowId=n/a variant=at-load scheme=http shape=named
```

## The 4.5 failure is the quiet kind

On webOS 5.4.2 a `load` without a window is refused (`error 104`). On 4.5 the same payload returns
`ok`, the pipeline fetches the file and decodes it — `videoInfo 1918x802` at 24 fps, audio, a
position that advances — and nothing appears on screen. Nothing errors anywhere.

So on 4.5 a missing picture means "the pipeline is playing for nobody", not "the probe broke".
Anyone repeating this must read `window=` on the `armed` line first: `window=none` on 4.5 is the
answer, and the black capture is its consequence.

## Reproducing it

1. Deploy a debug build that carries the probe (devtriggers) to the debug install.
2. Arm one variant in the install's runtime root, e.g.
   `echo load > /tmp/com.beb.plxnative.debug/plxnative-uriprobe`.
   A `plex.direct` video URL needs working name resolution on the set; a plain `http://` URL to a
   LAN server needs none and is what the 4.5 run used.
3. Play a title the route chooses to direct-play (the `armed` line says `kind=direct`), watch, and
   press Back.
4. Read `grep 'uriprobe:' /tmp/com.beb.plxnative.debug/plxnative-events.log`: `window=`,
   `playing`, `currentTime`, and the closing `summary` are the fields that matter.
5. Capture with the service screenshot — the app's own stream and any GL readback cannot see the
   hardware video plane, so a black rectangle there proves nothing either way.
6. Remove the trigger file, or the next Play probes again.

On firmware where the app's `/tmp` is jailed separately from a root shell's (reported on webOS 26),
arm the trigger through the app's own view instead:
`echo load > /proc/$(pidof plxnative)/root/tmp/com.beb.plxnative.debug/plxnative-uriprobe`.

## Open questions

- Whether webOS 22–26 permits a sideloaded app to register on `com.webos.media` and to have another
  process's pipeline import its exported window. Unmeasured; it is the question issue #1064 waits
  on.
- Whether URI mode plays formats the buffer-feed path refuses, on a stock set.
- Resume position, pause/seek, track selection and subtitle rendering through the URI session —
  none of these are in the probe.
- Whether the app's own overlays can be drawn over a picture the TV's pipeline owns.
