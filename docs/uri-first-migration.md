# URI-first playback migration

The target is a URI-first player with independently removable URI and buffer-feed executors.
This is a staged migration. Boot installs the URI executor and a source qualification port, but
the production qualifier returns Unknown and certifies no URI controls or release, except for a
viewer who turned on Settings > Video & playback > Playback engine > Experimental (off by default,
per profile): that opt-in is the viewer's own qualification of the original file's URL for their
own playback, for a file Standard would direct-play on that television (its video, Dolby Vision, audio and
container verdicts; not the server's own decision, which the offer is made before). Ordinary playback therefore
still selects buffer-feed. Injected source and firmware evidence exercise the URI route in host tests;
they do not qualify the production device.

## Dependency boundary

The final graph is application composition → shared `plx_media` orchestration plus optional
`plx_play_uri` and `plx_play_feed` drivers. All three use the small `plx_playback_contract` leaf.
Drivers may use lower platform/network/codec facilities, but cannot import `plx_media` or one
another. Neither driver owns Plex route selection, timeline writes or recovery policy.

`plx_media_codecs` owns the bundled FFmpeg ABI and loader, independent capture encoding and
audio conversion primitives. It cannot import either playback executor. Keeping a separately
enabled subtitle or capture codec consumer does not imply keeping the feed executor.

The URI product driver is composed into the adapter's one neutral `DriverSet`. Its separate
diagnostic probe retains a media facade with a route settlement callback. The shared runner
sequences load readiness, addressed track confirmation, exact seek completion and the desired
foreground Play goal. Its local metrics distinguish logical requests, physical attempts, clock
progress and picture evidence. Queuing work is never counted as a native acknowledgment.

The application composes the common runner with the URI and feed drivers. Addressed resource
mapping, timeline/Plex writes and recovery stay common. `plx_play_feed` owns demux, queues and
native state and physical execution in `native_executor`. The `media/player/engine` module is
the common route and telemetry bridge to that executor. Source IO and demux observations travel
through injected ports.
Caption stores receive explicit playhead and seek-target facts rather than reading feed clocks.
Both playback drivers are optional Cargo dependencies, and the URI-only normal dependency graph
excludes `plx_play_feed`. URI-only and Feed-only host source checks have passed checkpoints. The
delete-and-restore drill of 2026-11-06 (`docs/measurements/backend-removal-drill-2026-11-06.md`)
deleted each driver crate in a disposable checkout and built, tested and packaged the survivor; both
survivors passed every check in its sequence, the URI-only and Feed-only ARM packages included. What
remains is that record's "What failed or was left over" list (the `cfg` sites that stop a feature name
leaving the application manifest, the eight Python CI pins on the backend lists, the stale prose and
the untested Feed integration tests of the `nofeed` checkout) and a re-run of the drill on the
promoted tree, with the combined tree's full feature matrix, since the drill ran on an earlier base
and ran no `make check`. Source compilation alone does not prove
physical removability. `plx_media_codecs` remains required for independent codec consumers.

## Ordered checkpoints

1. Introduce the neutral contract and characterize addressed dispatch, sparse observations,
   stale replies, refusals and release quarantine. A queued operation never proves playback,
   picture presentation, track selection or seek completion. The existing route owner supplies
   attempt identity; the contract does not create a second logical playback owner.
2. Move shared timeline/Plex reporting, queued successors, track identity and recovery out of
   native execution. Split player state into common observations and private feed resources.
   Pass resolved audio/PCM/transformation decisions into the driver instead of reading routes.
3. Extract independently used codec operations. Keep one FFmpeg table and ABI validation;
   separate subtitle cues/anchors from demux state and use owned cancellable IO for extraction.
4. Break the adaptive cycle **before** moving the feed implementation. Move the live controller
   and propose/grade/commit loop out of FFmpeg into one common adaptive coordinator. Feed reports
   acquisition evidence and queue geometry and physically applies candidates. Route capacity
   sampling uses an injected runner, and pure ABR models receive geometry as values.
5. Extract URI protocol, ownership and driver code; split genuinely shared display primitives
   from feed-specific native operations. Once no upward edge remains, move the remaining feed
   engine, transport, segment reader, demux, queues and transformations into one feed crate.
6. Remove compatibility facades, compose drivers at application boot and enforce the optional
   production build/package matrix. Only then qualify and promote URI-first routing.

Each checkpoint must compile independently. Do not move the old media SCC wholesale into the
feed crate: that would move shared policy and preserve the cycle under another name.

## Adaptive commit and native ownership

The current HLS publication serializes queue abort, active-route validation, controller commit
and the worker ticket replacement. Preserve that linearization while replacing the old bridge.
A grant is not an active route. The physical apply must win the current attempt/route/cancel
fence and produce a sticky committed receipt; the common projection must reconcile a lost
receipt. Fence competing route mutations during this transaction. Test cancellation after grant
but before apply, stale seeks, delayed PMS answers and exactly-once losing-encoder cleanup.
Never hold queue/route locks while waiting for IO or a main-thread answer.

One common owner admits an attempt and holds the display lease until physical release is proven.
Backend workers and pending native resources stay private. Timeouts do not prove release. Keep
quarantine and the shutdown rule that avoids SDL teardown while firmware workers remain alive.
The ACB plane is released through the plane owner (`platform/src/tv/plane.rs`): `UNLOADED`, then a
fence on that call's completion callback (`docs/measurements/acb-release-fence-2026-11-04.md`), so a
released plane is reusable in the same process. Only a failed fence (a refused unload, a completion
that is not "complete", no completion within `RELEASE_FENCE_MS`) quarantines it until process exit.
The production URI backend has the binding (`Binding::Acb`); only a viewer's own Playback engine opt-in qualifier routes to it.

## Routing and evidence requirements

Prefer URI for qualified ordinary playback and preserve fidelity before permitted conversion.
Retain feed exceptions only where firmware and device evidence establish a need. Keep existing
Force Direct Play settings and original-stream handoff: no silent alternate track, DTS-core
extraction, PCM conversion, DV conversion or PMS conversion. Do not promise receiver bitstream
output merely because the application hands the original stream to the platform.

Qualify stock and `dts_restore_plus` environments separately. Preserve Developer Mode and
rooted/Homebrew installations. Install no patch and change no TV-wide audio setting automatically.
Native adaptive masters have one platform controller; fixed PMS offers have one app controller.
The pure `route::backend_policy` orders qualified original-stream URI offers before qualified
original-stream feed offers, with permitted conversion afterward. The common owner must supply a
finite offer catalog and stable recipe IDs, rather than treating a new transient URL as a new
candidate. Selection does not acquire a resource lease. Admission spends a recipe exactly once;
capability refresh preserves that history. At most one cross-executor handoff is permitted per
recovery episode. The route owner builds the catalog for the selected source; the application
installs an owned qualified URI source against its route ticket before start. The production
qualifier still returns Unknown for everyone who has not opted in (it answers a progressive
original-file source for one who has, only for a file Standard would direct-play (video, Dolby Vision, audio
and container; the server's own decision is not asked first), never a converted PMS output, and a URI failure
still takes the one handoff to Feed), and recovery integration remains under
test. This composition does not promote URI.

Each attempt starts with fresh observations. Unknown remains unknown; an acknowledgment or
advancing clock is not picture evidence. Separate logical-request metrics from backend attempts,
startup from rebuffering, and handoff gaps from ordinary stalls. Keep bounded typed errors and
independent reporting consent.

Both drivers reach one reducer (`attempt_metrics::AttemptMetrics`) through the one runner, and
each failed or rejected start, rebuffer episode, handoff and release failure becomes exactly one
local `Fx::Incident` record carrying the request, attempt, backend, stage and a closed incident
kind (`Failed(cause)`, `Rebuffer`, `Handoff`); the stage is only what the evidence proves and
stays `Unknown` otherwise. A handoff is recorded once, at the first of its begin and its
successor Start, and carries the source attempt's backend and stage. A release failure is
recorded by the physical-lifecycle owner (`backend_metrics`) once per attempt, even when its
verdict arrives after the logical request finished or was replaced, because the native release
outlives the request. These records stay in the adapter's bounded local window and add no transmitted field, so the
consent preview and privacy text are unchanged. A URI attempt's mid-playback `bufferingStart`
now moves the common state from Playing to Buffering, so the existing `playback.quality`
event's `rebuffers` and `buffering` classes can be non-zero for URI playback, where URI had no
source for them before; the handled error still carries the existing backend and terminal-stage
classes. The reducer counts a rebuffer only after
the request's first confirmed picture (`Observation::Presented`). The Feed driver emits it once per
attempt when the plane shows the picture (on webOS 4, the ACB bind is complete and a position tick
has followed it) and the sink's displayed-frame counter has reported a count above zero since that
bind; on webOS 5+ the first such counter callback is enough. This is necessary-condition evidence,
not photon proof, and a firmware that never delivers the counter leaves startup Unknown. A
position tick (`FRAMEREADY`) is never a picture. The URI driver emits it only on the ACB binding
(`Binding::Acb`, the webOS 4 plane), once per attempt, when the service has said `playing`, the plane
owner has seen the firmware complete BOTH the bind's full-screen `setDisplayWindow` and its
`setState(PLAYING)` with event 1 (matched on each call's own task id), and the service's
`currentTime` then advances past the sample before it (`play-uri/src/picture_gate.rs`). That is the
same necessary-condition grade: the plane was told to show the stream and the stream then moved. The
exported window and an unbound attempt emit nothing, and so does a firmware that never completes the
two calls. The rule is device-unverified: the log lines that confirm or refute it are
`acb display fullscreen=1 rv=1 task=N`, `acb playing rv=1 task=M`, the two `acb callback task=.. ev=1`
lines, `uri-acb: display/PLAYING completed` and then `uri: presented (...)`. The player's
transmitted `Presented` trace step and `startup` class are a separate, older value: they time the
first position tick of a Feed playback, not its first picture, and are unchanged. Preserve the existing local recorder limits and privacy policy;
new transmitted fields require schema, consent-preview and privacy updates. The closed attempt-origin domain includes `viewer_reconfigure` for a viewer-requested speed, audio-processing or source-track route change; it is listed by the same domain table in the consent preview. It does not add a payload field or enable reporting.

## Completion gates

Run committed-tree host gates, shipping-feature checks and ARM builds. Prove matched ABR
simulation trajectories and the transaction races above. Review firmware compatibility and
documentation claims before publishing changes.

Build both backends, URI-only and feed-only independently; host tests also support neither.
The Makefile now refuses `PLAYBACK_BACKENDS=none` for the television package. Audit production
features separately from test feature unification. The URI ELF import gate is present, and the
drill record holds the URI-only and Feed-only package inspections at its base; the same matrix on
the promoted tree is what remains. Inspect native objects, loader tables and packaged files: FFmpeg
is loaded dynamically, so ELF imports alone cannot prove exclusion.

In disposable verification checkouts, delete each backend directory plus only its workspace,
composition, native-build and package registrations. The other backend, shared orchestration,
UI and contract must still build without source changes. Restore using Git and repeat for the
other backend. A feature-off build alone does not prove physical removability. The drill record
above is this procedure run once; its "What failed or was left over" list is what a real deletion
still owes.

The [W19H subtitle handler inspection](measurements/uri-subtitle-handler-2026-11-03.md)
records the native setter/response ordering and its limits. It does not establish a public-service
completion event or enable a product capability.

URI promotion additionally requires independently observed picture/control/EOS and reusable
release evidence on the relevant firmware strata. Host compilation and synthetic transport
acknowledgments do not satisfy those device gates.
