# Playback diagnostics

Settings → Diagnostics is available in ordinary builds. Recording is off on every launch and is never enabled by a server, an account, a media file, or the diagnostics overlay. Select **Record playback diagnostics**, reproduce the problem, then select **Save diagnostic log**. Saving stops recording first and writes the combined log on the shared storage worker.

The recording stops after 60 seconds or 8 MiB, whichever comes first. The switch turns off automatically; the most recent recording stays local until another recording replaces it or it is deleted. Capturing detailed firmware messages adds formatting and disk IO, so recording is not a performance-measurement mode. **Delete diagnostic recordings** removes the trace and the saved combined log. Privacy & data → Delete all local data stops the writer and removes these files too.

## Collecting the saved log

The saved local file is `plxnative-playback.log` in the install’s runtime directory. It contains up to 8 MiB of the firmware trace and a tail of up to 256 KiB of ordinary app events. It uses the same token/identity scrubber and private log-file permissions as the existing support logs. It adds no uploader or network listener. Use the existing log-collection method (for example, the rooted TV’s file access) to copy the file for support; review its media/device details before posting publicly. There is no in-app downloader in this change.

| Install | Saved log |
|---|---|
| Stable | `/tmp/plxnative-playback.log` |
| Nightly | `/tmp/com.beb.plxnative.nightly/plxnative-playback.log` |
| Debug | `/tmp/com.beb.plxnative.debug/plxnative-playback.log` |

The underlying trace is `plxnative-gst.log` beside it. The saved combined log survives a relaunch, but runtime storage is not guaranteed to survive TV standby or reboot. Copy it before powering off. A new recording replaces only the trace; **Save diagnostic log** replaces the combined log.

## Implementation and firmware evidence

The Settings form emits typed `AppFx::PlaybackTrace` commands. The app runner executes them and delivers one `AppMsg::PlaybackTrace` status to the originating surface. The screen opens no file, starts no worker and reads no native/global recorder state. The platform adapter owns native logging and its file; the Bridge owns the off-frame save receipt and delivers its result to the requesting surface. Saving is generation-fenced against a later deletion or new recording, and the IO transaction is serialized with privacy deletion; the cap/deadline writer takes elapsed time as an input and is tested without firmware.

The adapter optionally resolves `libgstreamer-1.0.so.0` on the first explicit recording request. Missing APIs report Unavailable and do not block playback or startup. No GStreamer structure field is accessed. `GstLogFunction` and the public log/threshold API signatures come from the NDK `gst/gstinfo.h`; the six symbols used by the binding are checked against the DAS firmware generations 3, 4, 5, 6, 9, 10 and 11. Symbol presence is ABI evidence, not a claim that every generation emits every category.

The categories are dvbin, dvsplitter, dvsplitter_algo, dvmdparse, dualsequencer, lxvideodec and lxvideosink at level 6. A non-default callback writes through the strict byte/deadline guard. The default logger is removed during recording to avoid a second unbounded stderr copy. The app heartbeat disables native logging when recording completes, even if the firmware emits no further messages. Callback admission also checks the deadline, rejects recursive formatting and rejects records from an earlier recording generation. Closing a recording removes its writer before disabling logging; late callbacks see no capture.

The o20 `libpf` PlayerFactory::create path calls gst_debug_remove_log_function(NULL) and gst_debug_add_log_function(NULL, FILE*) only when both GST_DEBUG_FILE_OVERWRITE and GST_DEBUG_FILE are present. A normal build supplies neither. The general recorder uses a non-default callback and changes logging thresholds through public APIs rather than changing process environment after workers start. The old developer-trigger workflow remains a separate development instrument; it is not required or enabled by this feature.

Public API documentation: https://gstreamer.freedesktop.org/documentation/gstreamer/gstinfo.html . Firmware comparison and issue-1166 research are interoperability evidence; no recovered vendor implementation is included here.

## Verification boundary

Host tests grade off-by-default state, hard byte limits, deadline behavior, failed writes and typed UI effects. Native capture still needs a real television; the simulator can grade layout and the Unavailable state but cannot prove firmware logging. A successful recording does not diagnose or fix the issue-1166 Dolby Vision resume hang on another television.
