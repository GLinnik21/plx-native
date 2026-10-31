# player/ — the buffer-feed video engine

This is the in-process **buffer-feed** playback engine (was `src/playback.c`): it pulls the part
stream, demuxes it to access units, and `Feed()`s them to LG's StarfishMediaAPIs while `libAcbAPI`
binds the decoded sink to the hardware video plane. The Starfish/ACB calls cross into C at the seam
`src/starfish.c` (outside this dir — edits there almost always pair with edits here); the **ABI and
bind-order gotchas for that seam live in THIS file**, below. `docs/agent-reference.md` carries only the
one-paragraph playback summary.

## Sans-IO

`lifecycle.rs`, `recovery.rs`, `release.rs` and the route planners `route/plan.rs` and
`plan_memory.rs` are machines: a step starts no worker, reads no registry and has no `cfg(test)`
fork. The engine, the pump and the drivers beside them execute their effects.

@../../../../docs/sans-io-guide.md

## Pipeline

In-process is the point: ACB can only bind an app-owned sink, which the earlier URI/out-of-process
path (`com.webos.media/load`, `start_playback()`) could not provide — that path is kept only as
dead-ish reference, and `docs/buffer-feed-plan.md` records the pivot (treat it as history, not
spec). The stream side: `PMS HTTP GET` → demux → per-lane access-unit
queues with byte-cap backpressure (`aq.rs`) → the pump `Feed()`s each AU to the Starfish pipeline.
The demuxer is **`ff.rs` — the libavformat the app BUNDLES (not the TV's), over a custom AVIO on
one of TWO transports** (design record: `docs/ffmpeg-demuxer-plan.md`; the hand-rolled `mkv.rs`
fallback is retired/deleted). **Which transport is decided in `ff::demux` from the part URL's
scheme, or by a plaintext open's redirect to https (`stream::redirect` hands that hop to curl)** —
`AvioState` holds a source enum and `read_cb`/`seek_cb` dispatch: `http` → `stream.rs`'s raw socket, `https` → `crate::curlio`. That matters *here* because teardown is a
different mechanism per arm: `engine::teardown` fires `stream::http_shutdown` at the socket **and**
`curlio::abort_active()` at the curl source's wake pipe, since a thread parked in `curl_multi_wait`
is not one any `shutdown(2)` of ours can reach. Exactly one of the two ever has anything to do. Ours ships beside the binary as `libav*-plx.so.*`, is `dlopen`'d by absolute path
and pinned to majors 63/63/61, and is built `--disable-network` with `file` as its only protocol —
so the AVIO is not merely how bytes reach it today, it is the only way they *can*. See the root
`docs/agent-reference.md` linking section for why. It
emits Annex-B video AUs (param sets prepended at each keyframe) and, for AV1, Section-5 temporal units (TD OBU first, the av1C sequence header and metadata OBUs in-band on keyframes; `/tmp/plxnative-av1obu` = notd,noseq,raw tokens varies it on a debug build), for VP9 the demuxed packet unchanged (a frame carries its own headers and libpf's `video/x-vp9` caps have no `codec_data`), and AC3/EAC3, ADTS-framed AAC, or DTS core audio frames,
and seeks by time via `av_seek_frame` (libavformat's own Cues index).

## Threading model (this is the whole ballgame)

- `engine.rs` — the **main-thread-confined** session object. Every ACB/Starfish *control* call
  happens on the main thread except two Starfish verbs: `Load` (on `load_thread`) and a real stop's
  `Unload` (on the `sf-unload` worker, below). `engine` spawns the workers below.
- **A real stop's `Unload` runs off the frame (`engine::PendingRelease`).** Measured on the dev set
  2026-10-15 (release build, mock, Guest): `StarfishMediaAPIs::Unload` alone held the frame that
  handled BACK for 113-150 ms on every return (`teardown spans:` line, `sf_unload:`); the sched
  trace put the main thread in D state in `ioctl` (~31 ms), then in `futex` (~74 ms), inside that
  call. `teardown` (real stop only: `for_reload == false`) now moves the native phase to
  `Releasing`, hands the Unload to an `sf-unload` worker with an `UnloadTicket`, and parks the
  worker on the adapter. The rest of the release — callback gate, epoch retire, ACB
  `setState(UNLOADED)`, D1 or quarantine, `destroy_window` — stays on the main thread and runs in
  `engine::poll_release` (a pure machine, `release.rs`, decides): on the first frame after the Unload
  returns (`run.rs` → `poll_release_frame`). Nothing blocks on the worker: a start over a running
  Unload returns `BufferfeedStartOutcome::Deferred` (the player reads Connecting) and runs the frame
  the tail has run; `pa.mt()`/`pa.split()` hand out the token at once while `player::sink()` answers
  `RefusingSink` (`player::FENCE`: every verb refused, the memoized queries real) for the `Releasing` phase; `shutdown` waits via
  `exit_native`, bounded by `RELEASE_DEADLINE` (10 s), and skips `SDL_Quit` when a worker is still in
  firmware. Past the deadline a start is refused with `FailureKind::ReleaseTimeout` (Try Again), and the
  object is leaked, never destroyed under the worker, until the Unload returns. The foreground restore
  follows a deferred start too (`ForegroundLoadStart::Deferred`: parked like a rollback flight, started the
  frame the tail has run via `poll_release`, failed typed at the deadline); nothing on the UI thread waits.
  When the exit finds a worker in firmware, `exit_native` leaks the parked release and any abandoned Load
  (`mem::forget`), so dropping the `App` cannot free memory the worker still reads. While
  `Releasing`, only that epoch's `UnloadCompleted` callback is admitted (it is D1's evidence) and no
  native session can begin. Reloads, suspend and the decoder-release park (`for_reload == true`)
  keep the synchronous release. The log reads `native lifecycle: off-frame Unload epoch=N released
  X ms after the stop`.
- `pump.rs` — the **main-thread pump** (was `bufferfeed_pump`): each frame it drives bind → Play →
  feed and services seeks.
- `threads.rs` — the workers beside the demuxer: **`load_thread`** (construct Starfish + `Load()`,
  which owns its own GMainContext) and **`timeline_thread`** (the ~10 s `/:/timeline` progress
  reporter). The **demux thread body is `ff::demux`** (spawned by `engine::start_bufferfeed`): open
  the part URL, read+convert packets, push AUs to the two lanes; **and service seeks** — it
  `av_seek_frame`s on `seek_to_ns` between two `av_read_frame` calls, which is the whole seek
  mechanism (nothing interrupts it; see the seek gotcha below).
- `shared.rs` — the engine's cross-thread transport, callback and clock state (each field replaces a
  C `volatile` global — `g_*`). Route ownership and route-changing intents are the separate
  process-wide authority in `route::PLAYER_CONTROL`. Put new state under the owner whose invariant
  it belongs to; never smuggle it through a raw static.

**The main-thread rule is compiler-enforced.** The seam is the `VideoSink` trait in `plx_platform::tv::sink` (`rust-modules/platform/src/tv/sink.rs`)
(Starfish-shaped: one method per verb, reached through `player::sink()`). `ffi.rs`'s `extern "C"`
declarations are private to that module and only `StarfishSink` there implements the trait on the
television; `ffi_host.rs`'s `HostSink` is the simulator's. Every verb but `load` and `unload`
takes a `task::MainThread` (the constant queries `feed_abi_is_cow`, `window_mode` and `window_id`
take nothing); `unload` takes an `UnloadTicket`, which only a `&MainThread` can issue and which is
`Send`, so the main thread decides when an Unload happens and a worker may carry it out.
`MainThread` is a `!Send` ZST the application mints once. Since phase 9 it mints exactly one
and `boot` MOVES it into `player::adapter::PlayerAdapter` (`App.adapters.player`), which also owns
the native session that was the `static mut ENGINE`: the seam is reached as `pa.mt()`, the session as `pa.engine()` / `pa.split()`, and a function that
touches the session takes `pa: &mut PlayerAdapter` where it used to take `mt: &MainThread`. Moving
any of it onto a thread stops compiling (the closure captures a `&MainThread`, which `task::spawn`
rejects), and two live `&mut` to the session no longer needs a convention — it does not compile,
which is what turned `pump`'s "reload REPLACES the ENGINE, so `eng` dangles" comment into a rule
the borrow checker keeps. Two intentional holes, both worth knowing: `load` takes **no** token
because `load_thread` runs it off-main by design (and `unload` takes only a ticket, for the
`sf-unload` worker), and `MainThread::assume()` is callable — so an
`unsafe` block inside a worker still defeats this. The rule for new code: take the token **iff**
you reach the seam, the adapter **iff** you reach the session, so a signature keeps meaning
something.

## Gotchas that bite (all verified in code)

- **C-from-C++ Starfish calls** go through `extern … __asm__("<mangled>")`. Each Load gets a fresh,
  16-byte-aligned `SfSlot` whose `object[65536]` is constructed in place by the ctor symbol. Slot
  addresses are never reused or freed: a late firmware callback carries only that address, so a
  retired slot stays in the registry until process exit. Call the destructor only through the
  gated teardown path and **never** hand the object to C++ `new`/`delete` (its real size is unknown).
  Methods returning a `std::string` use a hidden sret first-arg; read the `char*` at offset 0 (SSO)
  for short replies like `"Ok"`/`"BufferFull"`.
- **Dolby Vision and Dolby Atmos have their own document: `docs/dolby-vision.md`.** A DV node is
  emitted only when the boot-time configd probe definitely confirms hardware support and the seated
  profile's Dolby Vision setting is Auto (`ResolveEnv::dolby_vision_mode`, applied by
  `DolbyVisionMode::gate` where the route plans, to the presentation only). The route freezes the
  probe's own capability (for read-outs) + that gated presentation decision, and every
  Load/reload/recovery consumes the
  stored presentation rather than re-reading the later cache; `dvnonode` is the logged diagnostic
  exception. Atmos routing and ACB forwarding are independent and unchanged. The payload evidence,
  Profile 5 one-tick fix and instrument traps live in that document.
- **The Load's `adaptiveStreaming` ceiling is derived per session (`engine::sink_envelope`), and
  it was a 4K60 constant for EVERY codec until 2026-09-03 — which on webOS 10 refused every H.264
  stream: `docs/webos10-resource-allocation.md`.** Lab-measured 2026-08-27 on release 10.3.1: the
  pipeline allocates against the DECLARED ceiling rather than the bitstream, so the Load came back
  `num=601 Resource Allocation Error`, while the identical envelope carrying `"H265"` played in the
  same session, and a 1920x1080 H.264 declaration loaded in 117 ms. The rule now is exactly that
  measurement and no more: HEVC keeps 4K60; H.264 declares 1920x1080@60 when the session's widest
  raster (`route::sink_max_raster` — source for direct play, min(source, ceiling) for any fixed-quality delivery (HLS or progressive), the
  catalog's widest feasible actuator for Auto, since a rung commit never re-Loads) fits FHD, and
  stays 4K60 otherwise; the device table's per-codec row (`devcaps::Caps::{h264_row,hevc_row}`,
  frame rate included) clamps it only when the table was actually read. The `load:` line carries
  `max=WxH@F`, the pipeline echoes what it was handed as `smp_cb type=5`, and the pipeline tier
  grades both (`load_max`, `sink_echo`). **Also since 2026-09-03: a `type=18` before any picture
  publishes `load_failed`** (`sf_on_event_inner`), so an asynchronous refusal reaches the failure
  read-out instead of parking the player in Connecting on a black screen (lab report §3.5).
  **After a picture, one number acts too (2026-10-13): `type=18 num=600`, "VDEC resource is
  released by policy action".** A paused direct play left until the screensaver came on got it with
  no `LIFECYCLE: background`, and the dead pipeline refused every Play after it. It raises
  `decoder_revoked`; the app loop parks the session exactly as an app switch does
  (`run::park_on_decoder_release`) and the viewer's next key or click is spent on the foreground
  claim that rebuilds it at the saved spot. The claiming press chooses the clock (`run::wake_input`,
  whose doc is the table): PLAY, and a play/pause TOGGLE (PLAYPAUSE, or a pointer click on no control — on the
  player only that click is the toggle; a click on the scrub bar, a disc or a tab is a plain wake) over a save that was Paused, claim with `ForegroundInput::PlayKey`
  and come back Playing at the exact saved offset; every other press, PAUSE included, claims with `DidForeground`
  and keeps the saved clock. OK is deliberately one of those: it is also how a viewer dismisses
  the screensaver, and whether that press reaches the app is unmeasured. A toggle over a save that
  was Playing is spent as a wake (the machine has no claim that restores a Playing save Paused).
  A Playing restore of a Paused save also clears the pause the park left standing BEFORE the Load
  (`PlayerForegroundActuator::before_load` → `transport::commit_viewer_resume_for_load`):
  `ClockStartGuard` arms the Load's initial hold from `TX.paused` (only with no live Engine: over one, `before_load` leaves the pause for `PlayClock`), and under the stale pause that
  hold was the viewer's, which fenced the first Play behind a prime (`SMP Play fenced; priming
  before retry`) and left the actual Play to the late `PlayClock`. That cleared pause, not the
  order of `playback_tick`, is also what keeps the paused-seek preroll shut. The app-switch PLAY
  key (`key_play` over a Paused snapshot) takes the same path. Every other `type=18` after a
  picture is still only logged.
  **And the frame rate IS a discriminator, measured on the dev set the same day:** a 4K H.264 24p
  direct play declared at 60 makes the pipeline announce `frameRate:24` and then `:30` for the same
  stream (a 24p picture on a 30 fps lattice — the judder the maintainer saw on the Auto 4K rung);
  declared at 24 or 30 it announces 24 once and holds it — and the sink's displayed-frame counter
  measured the picture: **13.0 fps presented under the 60 declaration, 24.1 under 24**. So H.264
  declares the stream's rate class
  (`fps_class`, with a 1 ms Matroska-timebase tolerance: 30.303 declares 30, issue #665; the esInfo rational stays the raw rate), HEVC keeps 60 (4K HEVC under 60 holds 24), and a stream with no known rate
  keeps 60. A REMUX (the enhanced `start.mkv`, `directStream=1`: video copied, audio re-encoded) copies the
  source's video stream, so it declares the SOURCE rate (`route::video_copy_fps`, from `Plan::src_fps` /
  `PlaybackSession::src_fps`) exactly as direct play does — until 2026-10-18 it carried 0, declared 60 and
  presented ~16 fps on a 4K H.264 24p file; a real video re-encode (an HLS rung, `no_video_copy`, a forced
  burn) still has no known rate and keeps 60. AV1 (only on a set whose codec table lists an AV1 decoder) keeps HEVC's 4K60 rule,
  clamped by `Caps::av1_row`; the clamp is unmeasured (one owner reports AV1 playing on a webOS 10.3.1 set, with no log). `/tmp/plxnative-sinkmax=WxH@F`
  overrides the envelope for the legs still unrun on 10.3.1.
- **There IS a presented-frame instrument now, and it is not a GStreamer trace.** The payload's
  `streamQualityInfo` / `streamQualityInfoNonFlushable` keys make libpf read the video sink's
  `dropped-frames` / `non-flushable-displayed-frames` properties on the same 200 ms timer as the
  position tick and forward them as callback types 46 / 47 (decompiled from libpf
  `CustomPipeline::updatePeriodicalInfo`, 2026-09-03). They land in the event log as
  `smp_cb type=47 num=<frames shown in the last poll>` (per-interval on this firmware, ~5 at 24p)
  AND, normalised for the numbering shift (46/47 here are 48/49 on webOS 5+ — `sink_counter_kind`),
  as `sink: displayed=<n>` / `sink: dropped=<n>`, which is the line the harness reads;
  `tests/run.py::presented_fps` turns them into the `presented:` characterisation line every
  synthetic case prints. Codec-agnostic — it is the instrument `dualsequencer:6` was only for
  Dolby Vision. Type 46 is emitted only when non-zero, and a stream shown at 13 fps reported 0
  drops: the sink does not count a frame it never presented as dropped. A session that took 25
  position ticks and never one displayed-frame callback logs `era callbacks: no displayed-frame
  counter ...` once (`era_callbacks.rs`): the numbering is chosen from the firmware major, and that is
  the symptom of choosing it wrongly. `docs/webos5-port.md` §5a is the table of every such private,
  era-dependent choice with its self-check and fallback.
- **A constructed Starfish object is not dispatchable until synchronous `Load` returns.**
  `sf_ready()` still answers whether the object exists; `sf_ready_object()` also requires the
  C `LOAD_RETURNED` gate. Rust records the return on the exact native epoch before publishing
  the route result. The pump waits before even polling `sf_is_load_completed`, with separate
  20-second issued→return and return→loadCompleted budgets. Timeout has code `load_timeout`;
  firmware refusal remains `tv_pipeline`. Teardown after that timeout does NOT join a media
  thread still inside `sf_load` (that froze the SDL thread on BACK): the thread, payload and
  epoch are parked as `engine::AbandonedLoad` (Rust phase `Abandoned`), native starts are
  refused, and `reap_abandoned_load` runs the ordinary Unload → gate → retire → D1 release on the
  main thread once Load returns; a Load that never returns leaks its object. A refusal says why:
  a reload refused for the parked Load (`pump::settle_reload`) republishes `load_timed_out`, and a
  refused FRESH start sets `ps.native_start_refused` (`Option<StartRefusal>`), which `player::state` derives `Error` from,
  because no Engine exists to publish it: cause `load_timeout` when a timed-out Load is still parked, `release_timeout` when the previous object's off-frame Unload outlived `RELEASE_DEADLINE`. Both offer Try Again; the first start that is not refused (`start_bufferfeed_for`, which every reload shares) clears it. A Load that has not
  timed out is still joined. The host concurrent tests model this boundary, not
  the firmware's native initialization. ACB dispatch is additionally constrained by stage/bind
  ordering; the C Starfish gate does not wrap ACB calls.
- **A flight HOLDS presentation for its worker's run (`claim_hold.rs`; the flight itself is
  `route/flight.rs`, whose `//!` states the owners, the discard rules and the suspend and
  trial invariants; the gates ask `route::flight_outstanding`/`flight_is_current`).** The PMS half
  of a claim (track pick, enhancement toggle, quality change, Original pick, adaptive refresh, the
  Auto watchdog's handoffs) and of a seek on a transcode runs on a worker for 1-15 s, and the
  landing reloads at the offset captured at dispatch, so a stream left playing would show those
  seconds twice. A seek's flight owns `Preparing(serial)`, a claim's owns `Applying(serial)`; the
  HUD reads Seeking through a seek flight (`player::state` ranks it above the hold) and Buffering
  through a claim's. `pump` calls `claim_hold::engage` when a claim dispatches `Pending` (the
  viewer's own `player::pause`, so `player::state()` answers `Buffering` and the existing
  transport spinner draws; a seek flight engages the same hold from
  `start_transcode_seek_flight`) and `take` + `release` AFTER `run_claim_tail`, so an accepted
  claim's Play lands on the new stream and a rejected one's on the kept Engine. A viewer press
  (`player::transport::set_transport_paused` -> `note_user_transport`) forgets the restore: the
  viewer's last transport press always stands. A stream already paused at claim time stays paused.
  While the hold's pause stands the viewer's transport reads see PLAYING
  (`player::transport::viewer_paused`, `claim_hold::owns_pause`): the OK toggle means Pause and a
  seek's `resume_if_paused` leaves the hold alone (its seek is carried past the reload by
  `pump::commit_or_carry_seek`, including a seek pressed mid-flight when none was pending at
  claim time). A reload (or a Pause pressed before the first frame) under a VIEWER Pause leaves a
  session with `frames == 0` behind a shut feed gate; `pump::settle_paused_seek` opens the bounded
  paused-seek preroll for exactly that state (paused, no claim-hold pause, no flight, no pending
  seek, no frame yet), whichever route reloaded, so the stream presents one frame and re-pauses
  instead of showing "Buffering…" over a blank plane. Where the preroll cannot open (the clock is
  not held yet), the pump ladder still never publishes Buffering for a paused, idle player with no
  picture (`pump::paused_and_idle`): Buffering means work is happening, and Playing means a frame
  is presented, so that state is `PlaybackState::Paused` (not busy, not `is_playing`, no
  `PlaybackStarted`). The preroll opens only while the prime is still owed (`Engine::prime_play`)
  and is owned at 0: `frames` is per-session, so the first presented frame is the landing.
  **On the ACB route (webOS 4) the landing also needs the video plane BOUND**
  (`pump::plane_shows_the_picture`): the plane connects only through `setMediaVideoData` +
  `acb_start` (`Stage::Streaming`), and a tick counted before that is a picture the panel never
  showed. A preroll re-paused on the first tick left the panel black ("no signal" on a VIDEO-only
  capture) until Resume, because the pause stops the ticks the bind waits on and
  `acb_mirror_playstate` does nothing below `Streaming`. So the re-pause waits for the bind plus
  one tick after it (`Engine::bound_at_frames`), and, while a preroll is open, the bind gate is one
  tick instead of the normal two (`acb_bind_frames_needed`; the preroll's Play window is audible,
  so `try_prime` mutes the pipeline first: `mute_for_preroll` calls
  `setVolume({"volume":0})` and logs `preroll: volume set 0 ...`; `getVolume` is NOT bound (it
  returns an int on 4.5 where the NDK header says `const char *`: SIGSEGV), so the volume is
  restored to the pipeline default 100 (`preroll: volume set 100 (restored; no readback)`) by `restore_preroll_volume` from `resume`
  (before its Play), the accepted re-pause in `finish_paused_seek`, and the pump's per-tick sweep
  for a preroll closed with no adapter in hand (`abandon_seek`). A stop or reload needs none: the
  volume is per pipeline and `TX.reset` forgets the mute. Never `setAudioMute` (absent on 4.4.2 /
  4.10.0), `AcbAPI_startMute` or the system mute). An `acb_start` that runs under a really-paused pipeline (no preroll
  open) is followed by the ACB pause, so ACB is never left PLAYING over a paused pipeline. The
  `seek: paused frame restored` line therefore means the plane was bound when the pause landed.
  An engine failure during the flight publishes `Failed` at once
  (`fail_current_engine`) and clears the hold, so the error read-out shows instead of a spinner.
  The encoder a claim replaces is NOT stopped by the worker: the pump retires it
  (`route::retire_superseded_encoder`) after the reload.
  - **An AUTOMATIC claim holds only when its worker reaches PMS.** The HLS-to-Original handoff
    dispatches `Pending` for a replacement remux (PUT + `/decision`, landing at the offset the Auto
    worker published) and `PendingNoHold` for a direct Original the worker already sampled, which
    needs no round trip and keeps playing. A refused one reopens the retained HLS
    (`pump::land_automatic_original`) because the Auto worker exited to hand the action over; the
    reducer remembers the refusal against that HLS encoder (`PlayerControl::original_refused_on`;
    an admitted trial whose Load fails is recorded the same way, on the encoder its rollback's
    rebase installs), so the reopened worker's source gate starts held (`ProbeBlock::RouteRefused`) until an HLS
    commit, a seek's rebuild or a new playback changes the route — no timer, no re-ask loop. The
    Original-to-HLS fallback (`route::execute_auto_hls_claim`) never holds, though its worker reaches
    PMS: the Original plays until the landing replaces it, and a refusal (`pump::land_automatic_hls`)
    raises `demux_io_failed`. A user pick made during an automatic flight WAITS in `pending_user` and
    is claimed after the landing.
  - **FAILURE-path recoveries are `FlightOwner::Recovery` flights** owned by the failed start
    (`Preparing(serial)`): the rollback's rebase after a failed Original trial
    (`route::dispatch_rollback_rebase`), the HLS fallback of an Auto Original that never opened
    (`route::dispatch_unopened_auto_hls`) and the facet conversion: the rebuild that has the server
    convert the facet of a pipeline refusal: the audio outside `aac`/`ac3`/`eac3` (not only native
    audio), else the source's video at a raster above 1080p, which an ENCODE with no video copy
    converts, or the manual both-facets encode (`route::dispatch_refusal_conversion`, below). They hold nothing (the Engine that failed is not
    playing) and the viewer keeps the connecting spinner (`pump::hold_connecting_state`). The
    landing is drained AHEAD of the failure branches (`route::take_ready_recovery_flight`, then
    `pump::land_recovery_flight`), because the branches' failure flags stay up for the whole flight
    and `fail_current_engine` would end it. `land_recovery_flight` fails nothing itself: it reloads
    onto the route, or hands `FlightRefused` to the machine. A refused facet conversion goes to the
    HLS rung for a cold Auto Original and otherwise, for a manual pick, to the both-facets encode when it is
    earned and to the app-decode rung (below) when it is not, the two in the order the source's width sets (a
    source of at most two channels takes the app decode first; a surround source never does, the app being its
    last rung), else to the read-out; a refused both-facets flight (the HLS contingency or the manual
    encode) goes to the app-decode rung, which a surround source's ladder has kept for last, or the read-out,
    a refused rollback is always the read-out.
    `route::recovery_flight_outstanding` is how the pump waits.
  - **Two rollbacks have no Engine for a pump to drive** (`route::dispatch_engineless_rollback_rebase`,
    marked `flight_engineless` beside the recovery marker): the foreground restore's
    (`recover_failed_foreground_original`) and a synchronous Load construction failure's
    (`start_original_trial_reload`). The HUD reads `Resolving` through
    `route::engineless_flight_outstanding` (the same answer a cold resume gives); the landing is
    drained by `pump::land_engineless_rollback` (the foreground machine's `land_recovery`, or the
    app loop's `pump::drain_engineless_rollback` when no foreground machine owns it) and reloads
    onto the restored route.
  - **An app-switch suspend drops the outstanding flight of every owner**
    (`route::begin_engine_teardown(true)`): a seek's or a recovery's start transaction goes back to
    `Stable` (a trial's own `Prepared` inside an Original trial), and a CLAIM's `Applying` settles
    to `Stable`, releases its hold and hands the viewer's pick back to `pending_user` for the
    restored Engine to claim (an automatic claim owes nothing). The landing is discarded either
    way, stopping only what its owner kind registered; a worker that has not posted yet is reaped
    by `route::discard_stale_flight_landing`. The suspend's position snapshot
    (`player::intended_pos_ns`) reads an outstanding seek's target, else a recovery flight's own
    offset (`route::recovery_flight_offset_ns`, since a resume that has not landed has seeded no
    clock), else `playpos_ns`.
  - **A recovery flight flies over an Original TRIAL's transaction without erasing its
    rollback**: an OS suspend mid-trial leaves `OriginalTrial(Prepared)` and the foreground
    restore's resume (`begin_resume` -> `route::dispatch_resume_rebase`) takes that transaction as
    `OriginalTrial(Preparing(serial))`, the trial's own flight phase (the fence is the same; the
    snapshot is `pending_original`, which no flight writes). A landing returns it to the trial as
    `Prepared` (the candidate's projection and remux follow the rebuilt stream), a refusal as
    `OriginalTrial(Failed)`, a second suspend as `Prepared`; a failed open of the rebuilt Original
    still rolls back to the retained HLS. A trial that is `Starting` or `AwaitingFrame` is refused.
- **The k5lp/k3lp sandbox preflight refuses native playback when `/dev/rtkmem` is unreadable.**
  The device fact is cached at boot, while the refusal belongs to `PlaybackSession` and clears
  on exit. Explicit Repair confirmation spends `Player.repair` once for the whole app lifetime;
  `PlayerAdapter` owns the worker receipt. Success still requires a full app relaunch. See
  `docs/native-video-sandbox.md` for limits.
- **Starfish `Load` must be constructed with `uid = NULL`** (`SMP_ctor(slot->object, NULL)`), and in
  buffer-feed mode the app must **not** `LSRegister` its own `com.webos.media` client — either
  collides with the pipeline's uMS connection (CONN_FIND_ERR). See the comment in `load_thread`.
- **ACB bind order matters** (mirrors Kodi/ss4s): `setSinkType(MAIN)` → `setMediaId` →
  `setState(LOADED)` → *wait for decoded frames* → `setMediaVideoData(<sourceInfo envelope
  as captured; a PQ VP9 gets its HDR keys written in first, see below>)` → `setDisplayWindow` → `setState(PLAYING)`. The payload passed to
  `setMediaVideoData` is the **whole `sourceInfo` envelope** captured verbatim from the pipeline's
  callback (`sourceInfoRaw`), not a reconstructed one — with ONE exception: a VP9 stream whose container says PQ gets `hdrType` / `mediaVui` / `mediaSei` written in first (`player/vp9_hdr.rs`), because a VP9 bitstream cannot carry the transfer function, so the pipeline's own envelope says `"hdrType":"none"` and the panel stays SDR (device-measured on the dev set, whose ACB path is the only one that sends the envelope: the TV's `picture.dynamicRange` flips to HDR with the rewrite and stays SDR with `/tmp/plxnative-novp9hdr`; webOS 5+ has no ACB, so nothing is sent there and its VP9 HDR behaviour is unknown). The envelope is otherwise untouched, and an envelope that already names an HDR type is never overridden. Audio is owned by the pipeline — **never feed
  ACB an audio SINK or elementary stream**. That half is real and unchanged. Its long-stated
  consequence is not: `SOUND_ERROR_019` is a literal that exists in **no library on this
  television** (swept across ~70 harvested libraries including 92 MB of Chromium), and the clause
  has carried no evidence since the initial commit. **`AcbAPI_setMediaAudioData` IS used**, for a
  two-key METADATA descriptor — `{"audio":{"immersive":"ATMOS"},"context":"<mediaId>"}`, fired
  right after `acb_bind`, which is exactly where LG's own client fires it. Device-measured
  2026-08-21: `rv=1`, audio untouched (1600 AUs, `reply=O`, no error), and the set's own
  "Dolby Vision / Dolby Atmos" read-out captured on screen. Details and addresses:
  `acb_send_atmos` in `src/starfish.c`. `AcbAPI_setMediaVideoData`/`setState`/
  `setDisplayWindow` take a `long *taskId` out-param as their last arg — the 3-arg ABI is required
  (2-arg calls corrupt memory / segfault).
- **Opus goes to LG's own decoder only as far as the set is known to take it** (`ff_native_audio.rs`,
  `devcaps.rs`). What a set announces is layered, in this order and logged at boot as `devcaps: ...
  audio layers:`: the codec table, minus the codecs `/etc/gst/gstcool.conf` shows nothing can decode
  (`apply_gst_decoders`), minus `devcaps::EXCLUSIONS` (rows per firmware codename, each carrying its
  evidence: the C1's `kisscurl-koli` limits Opus to stereo, and records that Vorbis is refused),
  minus the refusals this install learned. Auto's own stereo ceiling for Opus
  (`OPUS_AUTO_MAX_CHANNELS`) stays as it is; the rows add a ceiling Force Direct Play also holds
  (`Caps::forced_feeds_audio_track`). FLAC, ALAC and LPCM are not native: the app decodes them and feeds PCM
  (`ff_pcm.rs`, `devcaps::APP_DECODED`), so a PCM Load never raises the NAMED refusal below
  (`native_audio::Kind` is Opus only). They are NOT outside the learner, though: `recovery::suspect_audio` excludes only
  aac/ac3/eac3, so a PCM Load refused before its first picture can take the facet rung (the server converts
  the track), and a proven conversion is stored by codec like any audio facet (`NativeAudioRefusal::of_facet`).
  After two such plays an ALAC or LPCM row can remove that codec from the set's value, as FLAC's already can.
  That is existing behaviour, left as it is. A hi-res track (96 kHz FLAC, resampled by the app to 48 kHz S32) is declared
  in the same shape as a 24-bit 48 kHz FLAC, and the facet is keyed by codec alone: two refused hi-res plays can remove
  `flac` from the set's value and with it the bit-exact 44.1 and 48 kHz FLAC. Accepted knowingly, not keyed by (codec, resampled). A stream the Load declared PCM for but the app has no decoder for
  (float or 8-bit LPCM under PMS's `pcm`) is refused in the demuxer (`ff::pcm_load_feeds_raw`) and ends as
  `demux_failed` -> `LoadRefusal::SourceFailed`, which names no facet, so the audio-only conversion rung is not
  reached: Auto Original falls to its HLS contingency, Force Direct Play shows the error.
  DTS, MP3, Vorbis and TrueHD are the lossy entries of `devcaps::APP_DECODED`, enabled **per row**: `Caps::app_lossy` (`AppLossy`) is `Off` in the assumed
  fallback, `Measured` in a table that was read (the rows whose `AppDecoded::measured` carries TV evidence: DTS only, the
  U5 core decode at 13.1% of one of four cores with 60 fps held; MP3, Vorbis and TrueHD are unmeasured, so not offered) and
  `All` under the DEBUG trigger `/tmp/plxnative-appdecodelossy` (read once at boot by `devcaps::probe`), which is also how an
  unmeasured row is measured. A row is DECODED IN THE APP only where it is in force
  (`Caps::app_decodes_lossy_track`, the one answer): the trigger puts every lossy row in force (even where the set has the
  decoder); otherwise Force Direct Play has no native path for the track on a set without a DTS row (webOS 10+, or a refusal
  the install learned) AND the track has at most two channels, or the recovery rung below put the codec in force (`Caps::with_app_lossy_for`).
  **The app never mixes a source of more than two channels down to stereo by default** (owner decision 2026-10-28;
  D1(b) and D2 as amended). `devcaps::app_decode_mixes_down(channels)` (channels outside 1..=2, so an unknown
  width counts too) is the ONE predicate, `Caps::app_may_mix_down` its gate, and both the planner and the recovery
  ladder read it: under Force Direct Play (D1(b)) a surround track the set has no native path for is NOT decoded
  in the app (`app_decodes_lossy_track` is false for it, in either arm, the codec being in force or not), so the
  Forced pick refuses it like any unplayable track (`ForcedFailure::Audio`, naming the codec; before S4 the same)
  and a pipeline refusal of it reads out (`RecoveryEffect::NameRefusedAudio`: LG's 601 names no codec and is also a video/resource refusal, so the read-out names the
  track the Load was fed only when the refusal is attributable to it (`OpenRecovery::refusal_attributable_to_audio`: an Engine with no picture, a codec the attempt's `Caps::native_audio_track(.., forced)` says the set does not play, not the app's PCM decode, and no video above `VIDEO_REFUSAL_FLOOR` that the facet rungs would suspect; Opus, DTS on a set with a DTS row and FLAC/ALAC/LPCM keep the generic read-out), then the same `ForcedFailure::Audio` sentence as the planner's up-front refusal; scheme of discussion #695 row 10); a track of one or two channels is no downmix and keeps the app decode. Under
  Auto (D2) the surround downmix is only the very last rung of the ladder (below), after every server option was
  refused, and stays labelled; Auto's default for a 5.1 track the set cannot take is the server conversion that keeps its channels.
  **One exception, inside a request:** once the app-decode rung has put a codec in force (`Caps::app_lossy_in_force`) for a track of at most two channels,
  the in-force arm of `Caps::app_decodes_lossy_track` admits a track of ANY width, so a later pick of a surround track of that
  codec (`commit_audio_route`, `audio_track_direct_plays`) is a native switch that is mixed down and labelled "played in stereo"
  WITHOUT that track's own server conversion or manual encode asked first. Admitting the mix-down only for a rung entered
  for a surround source would send that pick to the server, and a refused switch rebuild is a `ClaimTail::Rejected` ("current
  stream retained", `pump::rejected_tail`), not a `LoadRefusal` the `OpenRecovery` machine steps, so it would not reach the
  labelled stereo play the ladder gives an open. Not built; the next request starts from `start` again.
  The `appdecodelossy` debug trigger (`AppLossy::All`) is the one explicit override. Auto never decodes in
  the app on its own, and Force Direct Play's pick is three passes (`route/plan.rs`): (1) a native track that honours the intent
  (the selected track, or a same-language non-commentary sibling: `StandIn::SameKind`), (2) the app's decode of the
  intended track (at most two channels), (3) only then the native stand-in of any track. A native sibling wins over the app decode, and a
  native commentary or dub never does. An app-decoded track is never the set's own decoder's (the Load
  declares `PCM`: stereo 48 kHz S16 for a lossy codec; the track's own table rate and width for FLAC, ALAC and LPCM, or the rate
  they are resampled to as S32 below: `route::stream_pcm`), is announced without a table row, the track menu says a surround one plays in
  stereo (`route::audio_decoded_in_app`), and
  `ff_pcm::PcmDecoder` decodes it (DTS: the core only, `core_only`), downmixes by the frame's channel layout and resamples it
  (`ff_pcm::PcmConverter`, lossy shape). The resample is linear from a source below 48 kHz and a windowed-sinc low-pass decimator from one above it
  (`speed_audio::Resampler`, the one the speed path uses: a Kaiser-windowed sinc, `Kernel::FULL` = `zeros 41, cutoff 1.0, beta 12.8`, f64; flat within 0.01 dB to
  90% of the output's Nyquist and at least 120 dB down from `fout - 0.9 * Nyquist` up, measured: pass band within 1e-5 dB, worst alias -124.5 dB at 176.4 kHz and
  -126.7 dB at 96 kHz. A source of 4x the output or more that is an even rate first goes through exact 2:1 stages of the cheaper `Kernel::RELAXED`
  (`zeros 12, beta 14`: 25 taps against 83; flat to 0.45 * out, 120 dB down over `[R/2 - 0.55 * out, R/2]`, the derivation is on the kernel),
  one per halving while the rate is even and at least 4x the output, so 192 kHz to 48 kHz is half the multiply-adds of the one ratio-4 stage it used to be;
  what is left goes through one FULL stage of ratio at most 8 down to `out * 2^k`, then `k` exact FULL 2:1 stages (halved from the output up, so an odd rate gets one table, not one per stage); the tables stay under 6.7 MB for any rate). MP3 and Vorbis stay
  `audio_rate_bounded` (a 96 kHz Vorbis has not been through the TV), so a track above 48 kHz is the server's; DTS (a core is 48 kHz at most), TrueHD
  (decimated by the app) and FLAC, ALAC and LPCM (below) are exempt. **Lossless hi-res**: a mono or stereo FLAC, ALAC or LPCM track above 48 kHz is
  resampled by the app (`ff_pcm::PcmConverter`, lossless shape) to 48 kHz, or to 44.1 kHz for a multiple of 44.1 kHz (`devcaps::pcm_load_rate`, the one rule
  for the planner, Forced, the track menu and the profile), and declared S32 whatever its own depth, at unity gain, saturating at the i32 range,
  without dither or limiter; one log line per stream counts the clipped samples and the peak. A 5.1 hi-res track still converts on the server
  (the profile keeps `audio.channels <= 2` for these codecs). A table rate (8 to 48 kHz) builds no converter and stays bit-exact. TrueHD is
  decoded as FFmpeg's `truehd` decodes it, up to the 7.1 bed; an Atmos object substream is not rendered. Its access units are
  1/1200 s, so the demuxer gathers decoded blocks shorter than 10 ms into AUs of about that length (`ff_pcm::AuBatcher`; a DTS,
  MP3 or lossless block is longer and, while the stream is contiguous, passes whole; a gap in the content time ends the AU). The speed decoder (`SpeedDecoder`) mixes by the same layout mask
  (`ff::frame_channel_mask`), so 2.1, 3.1, 4.1 and 7.1 are right at a speed too. Where DTS is not
  in force it is native exactly as above. A Load whose DTS the app
  decoded names no audio facet in the failed-open ladder (`StreamAudio::app_decoded_lossy`): the pipeline was fed PCM, and a
  facet keyed `dts` would be stored as a refusal of the set's NATIVE decoder.
  A named refusal comes from a native Load that ends
  in `type=18 num=200 Audio Codec Not Supported` before any picture (`mod.rs`'s callback sets
  `native_audio_refused` only while `native_audio::fed()` says the audio was fed natively; the
  machine turns it into `RecoveryEffect::ConfirmRefusal { Evidence::Named }`, which the pump runs as
  `session::record_named_refusal`: one write to the install-scope `native_audio_refused` preference,
  stamped with the firmware `devcaps::learning_firmware()` reads). That preference is the ONE list of
  refusals: `devcaps` keeps no list and takes none directly, and `devcaps::caps()` is **write-once**
  (the boot snapshot, one `Arc` for the life of the process; nothing folds a refusal into it). A
  play's START value is derived at the app's play-start edge, once: `devcaps::start_caps` over the
  rows in force of the stored record (`session::peek().applied_refusals()`, which only ever removes
  native audio, `Caps::minus`), and the attempt carries it. A write is queued on the storage worker,
  so a row reaches the NEXT play's start once it has landed, not the play in flight, and the recovery
  below never waits for it: it rebuilds from the attempt's own narrowed `start`. A write that does not
  land (unwritable storage) leaves the record as it was, as it does for every preference: the next
  play loads the combination, is refused once more, and the ladder converts it, one refused Load
  slower. Under Force Direct Play a row in force refuses before any Load: the Forced audio gate
  (`route/plan.rs`, `ForcedFailure::Audio`) reads the attempt's start value, so the read-out names
  the codec and offers Switch to Auto. **A row is applied only on facet-clean proof** (S2a, #1046):
  a NAMED refusal
  (`num=200`, the stall) applies at once (its row has no `proofs`), but the DTS 601 of #695 names no
  codec, so it is learned the other way. The machine's facet flight lands (`FlightLanded` carries the
  `/decision` lanes, `route::plan::decision_lanes`), and only a flight of an AUDIO facet whose AUDIO lane is `Transcode`
  and whose VIDEO lane is `Copy`/`DirectPlay` (`recovery::proves`) is `Proving`, unless the request
  already showed a picture (a request that played the facet natively proves nothing about it: the
  machine's `pictured`); the first picture of that converted reload emits
  `ConfirmRefusal { Evidence::Proof }`, and a second Load before it (a track switch, a restore after
  Home, a seek reload) drops the pending proof. `session::record_refusal_proof` stores the proof
  as ONE counted proof for `(build, codec, raw channels)`. A row applies once it holds
  `LEARNED_REFUSAL_PROOFS` (2) proofs, so one rescued play teaches nothing and a second one changes the
  next start's `start`. A video facet flight, a both-facets rescue (the HLS contingency or the manual encode), a rollback, a Load timeout, a source failure and a
  conversion whose video was transcoded too prove nothing and write nothing; an unknown channel count
  (`0`) is never stored as a proof. A named refusal REPLACES a counted row (`proofs` becomes `None`,
  applied at once) and a proof never downgrades a named row (`Session::with_native_audio_refusal`
  merges; proofs add, capped). The failed open then
  recovers on the
  pre-picture recovery path every failed open uses, in the same frame: the request's `OpenRecovery`
  machine (`recovery.rs`, owned by `PlaybackSession::recovery`, driven by `pump.rs`'s
  `drive_open_recovery`) records a refusal that names natively fed audio and picks its rung in the SAME
  step (`ConfirmRefusal` then the ladder's effect; there is no confirmation state to wait in).
  **The ladder** (the order of `OpenRecovery::decide`): (1) a pending Original trial's
  rollback, which narrows the attempt first by the facet the refusal named, because its rebase, the next
  retry and a quality change must plan from the narrowed value; (2) under Forced Direct Play, the
  app-decode rung (6) for a track of at most two channels, else the read-out (a forced play is never converted, and the app never
  mixes a surround source down for it); (3) the **facet rung**, for a refusal that names the
  pipeline (`type=18` of ANY number, or the stall; not a Load timeout or a source failure) of an
  Engine that has shown no picture (`OpenFacts::no_frames`: the machine lives per request but a
  picture is per Engine, so a reload after the request's first picture, a track pick, owes its own
  open the same rung): `recovery::suspect` names, by elimination and not by the refusal's number, the
  stream the pipeline was fed: its audio when that is outside `aac`/`ac3`/`eac3` (Opus 200, and the DTS
  601 of #695, alike), else the SOURCE's video when it was fed whole at a known raster
  (`route::stream_video`; a numbered refusal only, and not for a cold Auto Original, whose HLS rung
  is already the ABR-managed video re-encode and takes the refused raster out of its attempt
  instead). The machine sets the attempt to `start` less that facet (`Caps::without`; an attempt plans and profiles from the one value the machine
  carries, `PlaybackSession::attempt_caps`, derived from the request's `start` on each rung's entry,
  never narrowed in place and never from a later read of the process-wide answer), and `ConvertFacet`
  has `route::dispatch_refusal_conversion` fly a `ClaimWork::Encode` under a `FlightOwner::Recovery`
  that rebuilds today's route (which now plans the audio as the server's to convert), or, for a
  video facet (`VideoLane::Encode`), an encode with `no_video_copy` under the lower of the route's
  ceiling and the 1080p rung's (`Caps::video_ceiling` lowers that codec's `raster_bound` to 1920x1088
  in the profile and the direct-play gate too). A video facet never proves and is never stored
  (D6(c)), and
  `land_recovery_flight` reloads onto it (`DeliveryReason::RefusalRecovery`; the landing re-grades the
  stored Original candidate (`regrade_original`): its audio, so the ABR climb does not offer the
  refused audio's direct play again, and its video, which a candidate the attempt no longer direct
  plays leaves dropped). No track pick runs in S1a: the replacement converts the track the viewer was on, and the
  AC3 sibling is only chosen once a learned row is in `start` (S2); (4) the **HLS both-facets rung**, a cold
  Auto Original's HLS contingency (`StartAutoHls`), whose attempt lacks every facet the request took
  out and every stream this refusal named (the refused video raster, the suspect audio); (5) the **manual both-facets rung**
  (`RecoveryEffect::ConvertBoth`, `VideoLane::Both`) is one ENCODE with no video copy
  (`directStream=0&directStreamAudio=1`) of today's route, from an attempt that lacks the audio suspect and
  the refused raster of a numbered refusal. A pipeline refusal (`Refused` or `Stalled`; not a timeout or a
  source failure) of a no-picture Engine earns it once the earlier rungs have nothing to give, when the
  source's video is still fed and not yet excluded, or an audio suspect still narrows the attempt. A
  refusal with nothing to lower (a 1080p frame-rate refusal) still earns the one encode, which keeps the
  route's own ceiling; a lowered video raster clamps it to the 1080p rung's. The encode's own refusal
  reads out (it feeds an encoded video, `OpenFacts::video == None`, and no audio suspect) unless the app decode
  (6) is still open for it, and a refused HLS rescue ends the same way. The converted route's video counts as fed whole only for an Original remux, so a
  manual re-encode flavour with no stored Original candidate gets no both-facets rung (conservative);
  (6) the **app-decode rung** (`RecoveryEffect::DecodeInApp`): a pipeline refusal of a direct play
  (`OpenFacts::direct`: no server session) that has shown no picture, whose audio
  the pipeline itself decoded (`StreamAudio::app_decoded_lossy` is false), of a lossy codec the START value's policy admits
  (`Caps::app_may_decode_lossy`), once per request (`in_app`), after a refused HLS contingency or a refused facet flight, or at
  once under Forced (a track of at most two channels only, `Caps::app_may_mix_down`). **The source's width sets where it
  sits (owner decision 2026-10-28):** for a source of at most two channels the app decode is no downmix, keeps the direct play
  and asks no server, so it runs BEFORE (5); for a surround source (more than two channels, or an unknown
  width: `devcaps::app_decode_mixes_down`, the planner's predicate) (5), which keeps the channels, runs first when it is earned
  and the app decode is the very last rung, after the encode was refused too (a refused encode flight is
  `app_decode_or_error`'s). The channel count is the SOURCE track's (`StreamAudio::channels`). It flies nothing and asks no
  server: the attempt gets that codec in force (`Caps::with_app_lossy_for`,
  applied after the facets, with the refused video raster given back since the reload feeds the same source video), and
  `pump::decode_in_app` reloads the same direct play (`reload_at`, logged `open recovery: decoding dts in the app (stereo)
  after type=18 num=601`), whose Load declares the PCM and so is planned the audio stack's PCM lead like any other PCM Load
  (`engine::load_audio`); a surround track it takes is labelled "played in stereo" in the track menu. It never proves a facet, and a flight after an app-decoded refusal
  never does either (`unprovable`); it flies no flight and so has no rescue to report (`ReportRecovered` names a flight's
  rung). A remux or transcode route and a refused app decode go on to (5) or the read-out. A rebuild rung for a refused remux or
  transcode, and a resolve-time `PlayVerdict::Server` refusal, are not covered;
  (7) the read-out.
  The facet and both-facets rungs together are bounded by `MAX_RECOVERY_FLIGHTS` (2) per request, the facet rung runs once, and the
  rollback is not counted. A facet `start` already lacks (`start.without(facet) == start`: a stored
  refusal that `devcaps::start_caps` took out of `start`) narrows nothing and gets no flight, so a
  conversion that is still refused cannot loop; a refused conversion dispatch goes on to the next rung
  too (HLS for a cold Auto Original, else for a source of at most two channels the app decode and then the manual encode when earned, for a surround source the manual encode when earned and then the app decode, else the read-out), and a refused rollback is terminal. The viewer
  keeps the connecting spinner: no read-out, no second play request. **The project hears about a
  rescue** (issue #1075): the machine keeps the request's first refusal that `LoadRefusal::reports`
  (a pipeline number, the stall or a Load timeout, never a source failure) with the stream it named,
  and the rung of the flight the open rests on; the first picture of such an open leaves it as ONE
  `RecoveryEffect::ReportRecovered` per request, which the pump hands to `report::refusal_recovered`
  and which becomes the handled `refusal_recovered` event (closed fields only: codec class, channel
  bucket, the number or `timeout`/`stall`, and the rung `facet`/`hls`/`rollback`/`encode`, the last being the manual both-facets encode) under the
  error-report consent and no other. A play that was never refused, a ladder that read out, an app decode, a
  source failure and a flight Home abandoned send nothing. **A Load the machine did not
  ask for re-opens the ladder:** `RecoveryEvent::LoadStarted`, raised by
  `engine::start_bufferfeed_for` (which every Engine start passes: a fresh play, a reload, the
  foreground restore), takes a machine left in `Flight` (Home cleared the flight with the Engine)
  or `Failed` (the read-out was up) back to `Opening` with its spent flights and excluded facet
  kept; the recovery landing's own reload finds the machine already moved by `FlightLanded` (a
  clean facet landing goes to `Proving`, any other landing to `Opening`): in `Opening` it changes
  nothing, and in `Proving` the FIRST `LoadStarted` is that reload and only marks it (the pending
  proof stays), while a second one is a foreign Load and drops the machine to `Opening`, the proof
  lost. A refusal the machine does not handle fails the Engine
  (`recover_failed_open`), never silence; in `Failed` that re-asserts the read-out once a frame
  until the Engine is released, which a pre-picture Engine (`frames == 0`) never is
  (`release_failed_engine`). **Who owns a refusal is who
  observes it:** the machine owns a
  refusal the pump sees AFTER a Load call returned (`type=18` once `route_start_status` read `Started`,
  a Load budget, a stall, a source failure). The Load CALL's own synchronous refusal (`StartFailed`
  with `load_failed`) of a Load the foreground lifecycle follows (`Engine::foreground_follows_load`,
  marked by `app/lifecycle.rs` wherever it starts following an attempt) is the lifecycle's alone:
  `pump` returns before the ladder (`settled_by_foreground`) and `ForegroundLifecycle` settles it as
  #1018's retry (`Prepared`, the Engine retired). A fresh play's synchronous refusal has no
  lifecycle behind it and stays the ladder's. A
  silent stall is learned too, for surround only (`native_stall.rs`, checked in the pump): a native
  Opus track with more than two channels, Load returned, no picture, both AU queues holding data and no
  AU accepted for `NATIVE_AUDIO_STALL_MS` (4 s; a slow server has empty queues) is learned and
  recovered the same way, and never after the first picture. The stall is a refusal of the same class
  as `type=18` (`LoadRefusal::Stalled`), so a stall of a combination the attempt already lacks takes
  the next rung (for a manual Original the both-facets encode while the source's video is still fed
  and not yet excluded, else the read-out); it used to return from the watchdog with
  nothing done and leave the viewer on "Buffering..." for good (TV run G, 2026-10-26). Garbled playback (no stall, no event)
  still needs a static row. The event log has `native audio: watching`, `STALL, no picture`, the
  `remembered` line, `devcaps: play start less learned refusals` (once per play whose start value
  a stored row narrowed), and once per play the fed packets' `max` and
  `p95` size (the C1's 6-channel Opus decoder stalls at packets of 1536 B and above).
  A learned row records the firmware BUILD it was watched on (`tv::device::Info::build`, nyx's
  `core_os_release`, e.g. `6.5.3-47`). A refusal is evidence only on its own build, so an LG update
  gets a fresh native try: the answer ignores another build's rows (and rows an older record never
  tagged), and the first refusal or proof learned on the new build writes the list back without them
  (`Session::with_native_audio_refusal`). The static rows are keyed on the codename and are not
  forgotten.
  Debug switches (debug builds, files under `/tmp/<app id>/`, read at app start):
  `plxnative-nativeaudiowide` ignores the last two layers; with `plxnative-nativeaudiolearn` beside it
  the exclusions stay ignored but refusals are learned and applied (the demo: a stall, a learned row,
  an automatic server-converted replay); `plxnative-nativeaudioforget` applies no stored row this run
  (a row recorded during the run is stored but does not apply either, until the next launch; the
  request in flight still converts). `plxnative-refuse601` publishes the
  firmware's `type=18 num=601` through `sf_on_event` once per play request, after the Load returned and
  before a picture (`pump.rs`'s `inject_refuse601`; the file is read only inside that open window, once
  the request is not yet refused), so a DTS item on the mock server reproduces #695 on the TV: the
  ladder converts the DTS in one flight and the converted reload is not refused again. Vorbis is not fed natively: LG's Vorbis element
  needs a caps `streamheader` and libpf's payload has no key for it (`setVORBISinfo` reads only
  channels and sampleRate; Opus has `opusInfo.streamHeader`), and a C1 refuses every Vorbis Load.
- **The Load payload's codecs must come from the `/decision` OUTPUT, not the source file.** A
  transcode changes the codec/rate; building the Starfish Load config from the *source* metadata gives
  the decoder the wrong description → **silent audio / glitches**. Read the output codecs from the
  transcode decision. For ADTS/HE-AAC, use the **CORE** sample rate from the AudioSpecificConfig (SBR
  doubles it). See `[[audio-payload-codecs]]`.
- **Subtitles are client-rendered here — the TV's HW subtitle engine is URI-mode only** and
  unreachable in buffer-feed. Image (PGS/VobSub) subs are decoded and drawn by us; styled
  ASS/SSA and EMBEDDED TEXT tracks (SubRip, WebVTT, mov_text, SAMI, ...) use the bundled libass worker
  (`ass.rs`, `ass_source.rs`): ASS with its original headers, events and attached fonts, a text track
  with a header `text_ass::header` synthesizes from the viewer's size/position picks and cues
  `text_ass::convert` turns into events (`ass_source::push_text`, `Origin::Text`; the cue's raw key is
  its dedup identity, so a seek re-read changes nothing, and the track's events are a newest-wins view
  of those raw cues), over the bundled Inter and per-script Noto faces (`ass/fonts.rs`, `docs/ass-subtitles.md`).
  The old caption path is gone: there is no `SHARED.sub_cues`, no `active_subtitle*` and no appkit
  `draw_subtitles`/`caption_page`; every text cue lives in `ass_source`. Sidecar ASS keeps the complete script rather than requesting a SubRip conversion. Don't expect the pipeline to burn or overlay them. See `[[tv-subtitle-engine]]` and the `plex/`
  soft-subs note. An image sub's rect coords are in **the subtitle stream's own authoring canvas** —
  1920×1080 for Blu-ray PGS but 720×480/576 for a DVD VobSub rip — so `ff::sub_canvas` reads that
  canvas off the decoder (via `avcodec_parameters_from_context`, no raw struct offset; the ABI proof
  is in its doc comment) and `appkit::player_hud::sub_screen_rect` scales the whole display set into the
  video rect. Assuming 1080p unconditionally is what made VobSub render as a corner postage stamp.
  **An EXTERNAL text subtitle (the `.srt` beside the film) is a third producer, `sidecar.rs`:** the
  demuxer never sees it, so it is fetched whole from PMS and kept in its OWN store — not
  `ass_source`'s window of demuxer cues, which the demuxer refills and a backward seek would empty. SubRip
  and WebVTT are charset-decoded (`sub_charset`) and converted on the fetch worker by
  `text_ass::script_from_srt`/`_vtt` to one ASS script (the synthesized header, `Origin::Text`, then
  the `Dialogue:` lines), which `sidecar::ass_source` hands to the same libass worker as an embedded
  text track; a pick change re-synthesizes the header lazily there (revision bump, identity kept). A
  script past `ass_script::WINDOW_EVENTS` lines OR `WINDOW_BYTES` is fed to libass a window at a time,
  and a script loaded in windows is exempt from the 8 MiB whole-script bound (a 4 MiB TIS-620 file is
  ~12 MiB once decoded to UTF-8); each window stays within it, except a single line larger than the bound, which the renderer refuses. The only thing the sidecar still
  draws itself is the on-screen failure notice (`sidecar::notice()`, `notice_shown()`; 6 s), drawn by the
  player screen with `draw_subtitle_message` (and `notice_shown()` is a term of its `clock_fingerprint`, so
  the expiry owes a frame). It is silent where the server burns the selection (a re-encode, or a remux whose embedded
  track the app may not read itself); over a plain remux the app draws it, and draws an embedded
  track too (`subside.rs`) when the link is local and the Part is 1..=30000 kbps. Which sidecar
  starts is `sidecar::restore_selection`'s one answer: the server's selected one, else the one the
  track memory chose for this episode (`Plan::memory_sidecar`, its key and codec read from the
  playing item).
- **A seek NEVER interrupts the demuxer.** The pump publishes the target in `seek_to_ns` and the
  demux thread — the only thread that touches the `AVFormatContext` — `av_seek_frame`s on it
  between two reads. Do not reintroduce an interrupt: the pump used to `shutdown(2)` the socket to
  break the read so the outer loop would reopen and seek, and it could not work, because our AVIO
  is **seekable** (`seek_cb` reopens with a byte `Range`), so libavformat treats the broken read as
  recoverable, calls `seek_cb`, gets a fresh connection at the same offset and reads on.
  `av_read_frame` never returns an error, so the reopen never happens and the seek never lands —
  it just runs out the stuck-watchdog on pre-seek packets and escalates to a full reload. This
  survived a long time because the test suite's cases inherited a server-side `viewOffset`, so the
  seek under test usually had nowhere to go (fixed 2026-07-28; see the
  `docs/agent-reference.md` testing note).
- **Playback speed is done in the app, fixed per Load.** LG's `SetPlayRate` cannot work here (its rate
  seek is reset to 1x by the buffer-feed source's next SEGMENT, a mid-play call freezes the pipeline,
  and libpf mutes audio off 1x), so a speed session declares PCM audio, `ff::SpeedDecoder` decodes and
  `speed_audio` time-stretches the sound, and every timestamp to the pipeline is divided by the speed
  (`player::to_sink_ns`, inverted on the presented-frame callback by `from_sink_ns`; an audio AU's
  sink pts also carries the PCM lead of the next bullet, added after the division).
  A speed change is a seek to the viewer's own position marked `rate_reload`, and while the speed is not
  1x every direct-play seek reloads in every mode (`pump::seek_must_reload`). `play_rate_permille` is the
  viewer's choice and persists across reloads; `applied_rate_permille` is the current pipeline's and is
  reset by `reset_session`. The feed throttle's run-on reads the applied one (`FeedPace::reference_at`).
- **A Load that declares `"audio":"PCM"` on one of LG's audio stacks has its audio fed later than the
  picture it belongs with, by an amount the stack decides; a set with no lead (`adecsink` on 2016/2017, not
  determined; Realtek; MStar; an unknown stack) gets none at all** (#1086, #1088, #1092, #1093; FLAC and every other app-decoded codec (a DTS the app decodes under the lossy gate
  included: `engine::load_audio` plans the lead from the name the Load declares, which is PCM for it), and a speed
  session at any speed other than 1x; a compressed Load, DTS the set decodes itself included, gets none).
  App-decoded DTS, TrueHD, MP3 and Vorbis get the lead of the set's stack exactly as FLAC does, but none of them
  was FILMED (the measurements below are FLAC on `liblxa`); the converter's own clock corrects the filter's lookahead
  (about a millisecond) in the stamps, but Vorbis' decoder delay (a half-block pts offset, `ff_pcm.rs`) is not corrected and its effect on sync is not determined.
  *Which stack:* `devcaps::audio_stack()` fingerprints it at boot (`devcaps::probe`, the `devcaps:` line's
  `audio stack=`) by the GStreamer sink PLUGIN, because each LG generation has its own sink plugin family in `/usr/lib/gstreamer-1.0/`
  (a 2018 image also ships the 2016/2017 one, so the probe order decides), no Realtek or MStar image has
  any of the four, and in the firmware images read directly (the 19 unpacked images, 2016 to 2025; LG
  chips, Realtek, MStar) `liblxa.so.2` itself is under the image's `/usr/lib` on none (it is in the BSP
  partition, `/mnt/bsppart/bsp/usr/lib`, found through `ld.so.conf`; the dev set's own firmware too,
  although its runtime stat of `/usr/lib/liblxa.so.2` answered yes: the runtime layout is not the
  image's). Each probe step is a stat, never a symbol lookup (our build links LG's libraries through stub
  `.so` files that define every symbol) and never a `dlopen` (it would run LG's initialisation in the
  app). The first present file decides, in this order (OTA IDs and releases are firmware read by hand and
  by decompile passes, cited as reasoning, never pasted): `libgstlxaudiosink.so` (`Liblxa`),
  `libgstlgaudiosink.so` (`LgAudioSink`), `/usr/lib/liblxa.so.2` (`Liblxa`), `libgstaudiosink_new-1.0.so`
  (`AudioSinkNew`), `libgstaudiosink-1.0.so` (`AdecSink`), none (`Other`). The bare `liblxa.so.2` stat is a
  fallback checked AFTER the webOS 11 plugin because its runtime meaning is not understood (no image has it
  under `/usr/lib`, yet the dev set answers yes): a 2022/23 OLED updated to webOS 11 that kept a
  `liblxa.so.2` plays through `lgaudiosink`, and reading it as `Liblxa` would give Bluetooth 250 instead of
  400, a needless bus read and a log naming the wrong stack.

  | Stack (label) | Present | Sets | The lead (`engine::pcm_output_lead`) | Evidence |
  |---|---|---|---|---|
  | `Liblxa` (`liblxa`) | `libgstlxaudiosink.so` (first), or `/usr/lib/liblxa.so.2` (third, after `libgstlgaudiosink.so`) | 2019-2021 LG chips (dev set W19H 4.10.2), 2022/23 OLEDs before webOS 11 (INFERRED from the `tools/fwcompat.py` inventories, which carry `liblxa.so.2.0.0` for W22O 7.4.0 and W23O 8.3.0 / 9.2.0 and hold no plugin; no such image was unpacked) | `tv_speaker` 400 and `bt_soundbar` 250 (both MEASURED), any other output or none 400 (not measured) | dev set 505 with +105 = 400; W21O 6.5.3: template 400 raw, no +105, effective 400 by the same convention, its per-output walk still there |
  | `LgAudioSink` (`lgaudiosink`) | `libgstlgaudiosink.so` | 2022/23 OLEDs on webOS 11 (W22O 11.2.0-74, W23O 11.2.0-3711), 2024 OLED (W24G 9.2.4), 2025 OLED (W25G 10.3.1) | 400 on every output, no read (not measured) | template `{150,150,400,5000,5000}`, 400 raw, no bias |
  | `AudioSinkNew` (`audiosink_new`) | `libgstaudiosink_new-1.0.so` | 2018 LG chips, webOS 4.4.3 (OLED C8 W18O, LCD SK8500 W18H) | follows the output like `Liblxa` (400 / `bt_soundbar` 250 / else 400), ALL cells not measured | `libkadaptor`: default DSP offset 475 with a +75 app bias = 400 (hand-verified; 4.5: 505 with +105, also 400); AUTO plus a mixer channel turns lip sync off (hand); a decoder-only -150 ms for a wireless output (decompile only) => Bluetooth 250 |
  | `AdecSink` (`adecsink`) | `libgstaudiosink-1.0.so`, checked AFTER `_new` (2018 ships both) | 2016 OLED 3.4.3 (W16M: element `audio_sink`, rank 256), 2017 OLED 3.9.2 (W17H: element `adecsink`) | NO lead (0, no read), logged `not measured on this chip` | 2016: default DSP offset 550, no per-output part found, the AUTO/mixer rule is not in the plugin (not determined). 2017: default 442/472 ms, app-offset bias unknown (so differential and DSP bias cannot be split), and a decoder-only per-output part (-100 DD, -150 WIRELESS). A 2017 measurement with `pcmavoffset` must therefore be taken per output: Bluetooth should differ by about 150 ms |
  | `Other` (`other`) | none of them | Realtek, MStar | none | see below |

  The label `adecsink` exists so a tester's log names the stack and `pcmavoffset` can measure it.
  `Unknown` is before `probe` (hosts, hostsim, tests). The `tools/fwcompat.py` inventories
  (`~/.cache/plxnative/fwsym/data`) are per-library symbol tables: they carry `liblxa.so.2.0.0` on 7.4, 8.3
  and 9.2 and hold no GStreamer plugin (`/usr/lib/gstreamer-1.0/*.so`), which is why the stack is
  fingerprinted from the plugin files and the firmware images are the evidence. One webOS version spans several stacks (the 2022/23 OLEDs are `liblxa` up to webOS 9
  and `lgaudiosink` on 11), so the firmware version cannot decide it.
  *Why Realtek and MStar get none:* their lip sync is NOT decoder-only (no mixer-off rule), so there is no
  decoder-only compensation for the lead to match; it is not that they use one sink for both paths (the
  `lgaudiosink` stack uses one sink for both too). What was read: Realtek's PCM path is in step on the
  speakers and its Bluetooth is not determined; MStar may be up to about 128 ms early (its SoC delay
  floor, the RTC path not verified by hand). A stack whose PCM path was already in step would be made 0.4 s
  late by 400 ms, so these get 0 ms (the behaviour before #1087) and the log says `not measured on this
  chip`.
  *The `lgaudiosink` stack (firmware, NOT filmed, no webOS 11 set exists here):* the same rule as `liblxa`
  (lip-sync mode AUTO is ON for the decoder and OFF for a mixer channel), and libpf's `CustomPipeline` never
  sets the sink's `lipsync` property (CONFIRMED on webOS 11: in W22O libpf every cross-reference of the
  string is inside `GenericPipeline::deepElementAddedCallback`). The template's 400 ms is raw (4.5 was 505
  with a +105 bias; webOS 11 dropped both), and the sink's own 475 ms render delay is applied on both
  paths and cancels. That the per-output table (`LIPSYNC_OFFSET_VALUES`) is unreferenced is checked in
  part only: on W22O it has no relocation and no `movw`/`movt` pair for its address and its getter
  `lgao_get_delay` has no direct call site (W24G the same); a pc-relative reference was not ruled out.
  Per-output adjustment moved to the VIDEO plane (avSync keys by `soundOutput`), which moves both paths
  equally. So the lead is 400 ms on every output, Bluetooth included, and the output is not read. If the
  table were read after all, Bluetooth would need 250 and this gives it about 150 ms too late.
  *The one link not proven on webOS 11: the `audio-port` value the `PCMMC` resource reaches the sink with.*
  The sink's rule is `audio-port == -1` and `mixer-port >= 0` => mixer. In W22O libplayerAPIs
  `acquireResources` retypes the ADEC entry to `"PCMMC"` in place and libpf `CustomPipeline::setResource`
  forwards ports verbatim, so the decision and the rule are the same as on 4.5, where the measurement
  shows the mixer path; the literal value was not read. If it were 0 or 1 the sink would stay off the
  mixer path, AUTO would resolve to ON, app PCM would get the decoder's own 400 ms and already be in
  step, and this lead would make every such set 0.4 s late. Check on a set: the sink logs `mixer mode
  changing %s -> %s` and `mixer-port is %d`.
  Owner decision: no re-encoding to a codec the set decodes itself; playback stays straight and lossless,
  which is why the lead is the mechanism.
  *Cause (read from the firmware, not measured):* a Load whose every audio track is `audio/x-raw` is
  allocated its audio as a `PCMMC` mixer channel instead of an `ADEC` decoder (libplayerAPIs
  `allocateResource`, libpf's resource manager renames the resource), and liblxa's `lxao_play`, in its
  automatic lip-sync mode, enables the DSP lip-sync stage for the hardware decoder and disables it for a
  mixer channel. That stage's template offset is 505 ms and every value an app gives it is biased by
  +105, so the decoder path is delayed 400 ms and a PCM Load is not. libpf's `option.lipsyncOffset` cannot
  stand in for the lead: it moves the audio and the video sink alike below 400 and only feeds the DSP
  stage that is off on a mixer channel from 400 up. *The per-output part (also firmware):* for its own
  decoder the set additionally advances the audio by a delay chosen from the active output
  (`_lxao_find_lipsync_output_type`: wireless 150 ms, Dolby bitstream 160, Atmos 170, DTS 160, WISA 100,
  ES 30) so that it covers that output's latency; a mixer channel skips that query and always reads 0. So
  the right lead is 400 ms minus the output's delay, which on Bluetooth is 250.
  *Filmed* (a FLAC clip with a flash and a beep every 2.5 s; offset = beep minus flash, negative = audio
  early). TV speakers: no correction -0.377 s; `pcmavoffset=330` -0.089; `=390` +0.047 (24 fps camera);
  `=400` -0.026 to -0.028 (30 fps), -0.03 to +0.08 in a later run; the set's own decoder on a DTS clip
  -0.02. Bluetooth headphones (`soundOutput` read `bt_soundbar`, phone microphone against an earcup): 400 ms
  +0.13 to +0.19 s, late; 250 ms (`pcmavoffset=250`) +0.017 s, three of three; the set's own DTS decoder
  -0.028 s.
  *The ONE correction* is `engine::audio_sink_pts`, at the audio lane's single Feed: the Load's
  `Engine::pcm_sink_lead_ns` is added to the SINK pts AFTER `to_sink_ns`, because it is real time and must
  not be divided by the speed. `engine::load_audio` yields the declared audio name and a `PcmLead` plan as
  one value (`Off` for a compressed Load; a speed override makes any codec declare PCM, so it plans a lead);
  `PcmLead::resolve` settles the plan against the output with the table `PCM_LEAD_BY_OUTPUT`, and
  `engine::pcm_output_lead(stack, output)` is the whole table (the stack table above): `liblxa` +
  `tv_speaker` 400, `liblxa` + `bt_soundbar` 250, `liblxa` + any other output or no answer
  `PCM_SINK_LEAD_NS` = 400 (not measured); `audiosink_new` the same three answers, none measured (its plan
  is `PcmLead::FromOutput(stack)`, as `liblxa`'s, so the stack the log names is the one that was read for);
  `lgaudiosink` + any output or none 400 (not measured; `PcmLead::Fixed`, settled on the frame thread with
  the lead in the Engine from the start, no read); `adecsink`, other and unknown stacks 0 whatever the
  output (`PcmLead::NoLead`, settled on the frame thread, no read).
  The `pcmavoffset` dev override (debug builds, read per Load) wins over the table AND the stack, and is how
  another output or another chip gets measured. Content time (`fp`, `max_fed_audio_pts`, `audio_start_fp`, stale-drop, throttle, prime) is
  untouched, so no producer of PCM stamps its own offset.
  *Where the output is read:* `start_bufferfeed_inner` runs on the FRAME thread (the Engine is
  main-thread-confined and the frame may not wait on the bus), so it cannot ask. A plan that follows the
  output goes to `threads::load_thread` (the `media` worker, which blocks in `Load` anyway), which reads
  `tv::sound::output` BEFORE `Load` (a `getSystemSettings {"category":"sound","keys":["soundOutput"]}` call
  through the `tv` port, 300 ms budget, `NoPlatform` on hosts), logs `pcm: audio sink pts +<N>ms (stack
  <liblxa|audiosink_new>, output <name|unknown>, default 400ms[, not measured])` once per Load (the
  `lgaudiosink` stack logs `pcm: audio sink pts +400ms (stack lgaudiosink, every output, not measured)` and
  a stack with no lead logs `pcm: audio sink pts +0ms (stack <adecsink|other|unknown>, not measured on this
  chip)`, both from the frame thread), and hands the lead back inside
  `LoadCall::Returned` (`Shared::native_load_pcm_lead`). Nothing is fed before the Load returns, and
  `settle_pcm_lead` takes the lead into the Engine, once, before the first audio unit; until then the audio
  lane waits rather than stamp with a guess it would later move. A compressed Load, an override and a stack
  whose lead does not follow the output (`lgaudiosink` and the stacks with no lead) never read the output
  (the last pay nothing on the bus). A cost to know: on a PCM Load the read sits directly in front of `Load`, so it adds its
  round trip to time-to-first-frame (its length is in the `sound: output ... read in Nms` line). *Worst
  case:* the 300 ms bounds the CALL only. `ls2::register()` before it and the unregistration after it are
  not bounded (`webos.rs`'s `ls2` doc), and `ls2` has no bounded registration to use instead, so a wedged
  luna hub blocks the Load thread in the registration, before `Load`, for as long as the hub takes: that
  Load is delayed by it, and the pump's `NATIVE_LOAD_BUDGET` fails it if the wait outlasts 20 s. A hub
  that registers and then stays silent costs the 300 ms and keeps 400. The answer moves the lead by at
  most 150 ms (400 to 250), which is why the call's budget is short. *Timing figures:* `native: Load
  returned after Nms`, `LoadElapsedClass` telemetry and `native_load_elapsed_ms` measure Starfish `Load`
  alone (`threads::load_alone` takes the Load thread's read out of them); `NATIVE_LOAD_BUDGET` still runs
  on the wall clock from `begin_native_session`, so it counts the read, on purpose (a wedged registration
  must fail, not park Connecting).
  *Seek (firmware, not filmed):* LG's audio sink hands each buffer's pts to the driver unchanged and
  re-derives its render base at every SEGMENT from the segment and the pipeline clock, never from a
  buffer's pts; no late-drop or gap rule exists. So the relative lead survives an in-place seek and a
  reload. An in-place seek keeps the Engine and its lead; a reload (a Load) reads the output again.
  *Filmed only on the dev set* (webOS 4.10.2, a `liblxa` set); nothing else was filmed. *Not determined:*
  the `audio-port` value the `PCMMC` resource reaches the `lgaudiosink` sink with (above: the sink's
  `mixer mode changing %s -> %s` / `mixer-port is %d` log lines settle it); the whole `lgaudiosink` stack
  (webOS 11, 2024, 2025: its 400 ms is read from the template and the 4.5 measurement, never measured there,
  and its per-output table is only partly checked as unreferenced); the whole `audiosink_new` stack (2018:
  every cell not measured, its Bluetooth 250 from a decompile only); 2016/2017's `adecsink` (no lead: the
  2017 offset cannot be split into differential and DSP bias and 2017 has a decoder-only per-output part
  (-100 DD, -150 wireless), so a `pcmavoffset` measurement there must be per output, Bluetooth about
  150 ms different; 2016's AUTO/mixer rule is not in the plugin); Realtek (Bluetooth not determined) and MStar (possibly up to about
  128 ms early, the RTC path unverified): no lead; on `liblxa`, any output but the two filmed (optical,
  ARC, wired headphones, LG Sound Sync, a soundbar with its own latency: they keep 400 ms and the log says
  "not measured"), and the newer firmware's mixer channel forced to type PCM (hand-verified on 4.5
  only); an output change in the middle of a playback, which takes effect at the next Load; the user's
  "AV Sync Adjust" menu, whose effect on a mixer channel is not known; a Bluetooth device whose own latency
  differs from the filmed one; another sample rate, chip or firmware. Also unfilmed: a seek, and the speed
  path (it takes the same lead from the same code).
- **Seeks are in-place**, in one of three per-session modes (`player::SeekMode`, chosen by
  `engine::select_seek_mode` in `start_bufferfeed`, the `/tmp/plxnative-seekmode` dev trigger
  `native|segment|reload` wins): the demuxer is `av_seek_frame`d to the target, then on the first
  post-seek keyframe `feed_stream` re-anchors the pipeline — no reload/decoder re-init.
  `Segment` (webOS 4.x) is Kodi-style: pump `flush()`, then `setTimeToDecode` + `sendSegmentEvent`
  through decompile-derived private offsets. `NativeSeek` (`VP_EXPORTED`, webOS 5+, issue #568; and a
  known webOS 3 release on the ACB plane, `engine::old_abi_seek_tested`) is
  LG's PUBLIC `StarfishMediaAPIs::Seek(millis)` (`sink().seek`, `sf_seek_ms`): it flushes itself, so
  the pump does NOT `flush()` and nothing may flush after it (a `flush()` is a seek to 0 and re-creates
  the stale-segment stall); it re-anchors the segment AT the millis passed, so `feed_stream` feeds
  content-time PTS (`pts_shift = 0`) and calls `Seek(max(kf_pts,0) / 1e6)` before feeding the landed
  keyframe — on the main thread, the only Feed caller, so it cannot race a Feed. A refused Seek
  (rv 0) drops the session to `Reload`, re-posts this seek for the pump's `reload_at` (the stuck
  watchdog goes straight to `reload_at` in `Reload` mode, and a flushed keyframe in a non-`Segment`
  mode anchors nothing: the private Segment offsets never run on webOS 5+), and latches
  `native_seek_refused` so the reload's fresh session stays `Reload` for the rest of THAT playback;
  a fresh play (`start_bufferfeed_tracked` with no live Engine) clears the latch. `Reload`
  (the fallback of both, every other layout-unknown library, and a set with no video plane) is a
  fresh `Load` per seek. Otherwise the mode is
  re-selected per session. The evidence is in `docs/webos5-port.md` ("The public `Seek`"). A transcode seek instead
  restarts the encode at `&offset` with a full fresh `Load`, and the PMS half of that restart is a
  FLIGHT, never frame-thread I/O: `route::dispatch_transcode_seek` plans on the frame, a worker
  registers the replacement encoder, and the drain installs it and reloads (`seek_landing_tail`).
  Taps that arrive during the flight coalesce to the last: the outdated landing is discarded
  (its replacement stopped, the encoder on screen untouched) and the next flight flies at the
  newest target. The COLD RESUME of a freshly resolved transcode (a saved `viewOffset`) is a flight
  too, `begin_resume` / `land_resume`: it owns the start transaction the landing left `Prepared` (a
  `FlightOwner::Recovery` — a start's route being rebuilt, with no Engine), the start stays owed
  (`player::state` answers `Resolving` through `route::engineless_flight_outstanding`;
  `play_pending()` is already false by then), and the app loop's drain starts the Load when it lands
  (`finish_landed_play`). So is the FOREGROUND restore's resume of a parked transcode: the
  foreground machine (`player/lifecycle.rs`) parks in `PreparePending` / `RecoveryPending` (both
  `awaiting_load`, so playback and the cold drain stay off), and `poll_foreground_flight` (`player/foreground.rs`) — once a
  frame from `playback_tick`, outside `playback_may_run` — drains the landing with the same
  `land_resume` and carries on into the Load; a refused rebuild re-arms the suspended snapshot, a
  flight that ended without landing releases the machine, and a SECOND OS background while it flies
  drops the flight (`park_pending_foreground_flight`) and parks the session as it was. Back, a
  suspend or a newer play request ends any of these; the late landing is reaped by
  `route::discard_stale_flight_landing`, which `playback_tick` runs every frame since there is no
  pump. `route::transcode_seek` is a test-only composition. The
  rebase machinery
  (`pts_shift`/`rebase_pending` in `shared.rs`) keeps Starfish from ever seeing a PTS jump.
- **`frames` is SEEK-scoped, `seen_frame` is SESSION-scoped.** `pump` zeroes `SHARED.frames` as
  *part of applying* an in-place seek (it counts only post-seek frames, for the rebind + resume
  re-pause gate), so `frames == 0` does **not** mean "we have never shown a picture" — it is true
  for the whole of every seek. `SHARED.seen_frame` is the bit that answers that question: set beside
  `frames` in the presented callback, cleared **only** in `reset_session`. The HUD divides its two
  busy indicators on it (`appkit::player_hud::busy_surface`); anything else asking "has this session put
  a picture on the panel" wants `player::seen_frame()`, not `frames() > 0`.
- **A remembered track choice is written back to PMS on the first decoded frame, never at the
  landing.** `route::apply_plan` only PARKS it (`PlaybackSession.write_back`: installing a URL is not
  evidence the television accepted it, and a refused direct play's rescue PUTs its own selection);
  `pump`'s `frames > 0` branch calls `route::on_decoded_frame`, whose last step is
  `route::send_parked_write_back`: it `take`s the value (one write per play), drops it when
  `parked_write_stale` says so (the route changed: another URL, any transcode; or the selection on
  screen moved), and otherwise only QUEUES one `PUT /library/parts` on the serial selection worker,
  so the frame never waits on the server. The queue stamps each request with the slot client's
  `(instance_gen, grant_epoch)` and the worker drops it when either moved: a profile switch re-tokens
  the SAME slot, so without the stamp a request queued under one profile would go out under the next
  (manual picks share the queue). A failed PUT is logged and never retried. "The selection on screen"
  is read from the SESSION once the landing has restored the server's sidecar, not from the plan.
  A subtitle change before the send that is not the viewer's pick (the first-frame container-name
  re-match, FO) re-points the parked write with `route::repoint_parked_subtitle` and queues no PUT
  of its own: a PUT of its own would move the selection under the parked write, which would then be
  dropped, and would reach PMS without W5 and W7. That re-match is
  `route::rematch_subtitle_with_names`, and the order of the `frames > 0` steps is load-bearing:
  re-match FIRST (the demuxer's track names exist only once the file is open), `send_parked_write_back`
  second, so one PUT names the remembered audio and the re-matched subtitle together. The order lives
  in ONE function, `route::on_decoded_frame`, which the pump and the write-back tests both call, so
  a swap there is graded (the two-step tests go red); do not re-inline the steps in `pump`.
  `repoint_parked_subtitle` runs BEFORE the commit, because the commit changes `cur_sub_sid` under the
  parked pair, and it asks the SAME staleness predicate the send asks (`parked_write_stale`) before it
  touches the write: a write the send would drop (route replaced in place, an audio pick before the
  first frame) is dropped there and logged, `repoint_parked_subtitle` returns `false`, and the re-match
  commits with its own PUT, so the server always hears the subtitle on screen. That PUT, parked or not,
  is W5-gated: over a Part that carries an external stream the re-match commit PUTs nothing (the
  selection moving off a download makes PMS discard it). With nothing parked it otherwise commits with
  its own PUT as before. It holds itself to the
  subtitle the landing put on screen (`owed.landed_sub`, settled from the session by
  `settle_landing_selection`), so it never replaces a remembered sidecar the restore drew.
- **App-switch lifecycle** (handled in `app/run.rs`, the frame loop; the machine is
  `player/lifecycle.rs`, its drivers (`drive_foreground` and the `poll_`/`park_` fns) are
  `player/foreground.rs`, and the transport-pause contract is `player/transport.rs`; `app::lifecycle`
  re-exports all three; details in the `docs/agent-reference.md` gotchas): OS
  background suspends the buffer-feed preserving the session. Foreground tracks one exact Load
  attempt at a time, follows reducer-approved superseding or rollback attempts, retries an exact
  failure without repeating route preparation, and issues the clock's native Play only after
  `Started` (a Playing restore over a standing pause publishes the viewer's resume BEFORE the
  Load, `before_load`, so the Load is not armed under it).
  The opposite edge is `ForegroundInput::Superseded(Successor)`: `exit_player` sends
  `Superseded(Exit)` FIRST, retiring whatever a restore parked (a refused Load's `Prepared`, a
  `Suspended`, a pending Load, via the `Retire` effect), and the accepted-resolve boundary in the
  frame loop sends `Superseded(Playback)`, which retires the parked states but leaves a pending
  Load to its own settlement. `DidForegroundSuperseded` is the DID edge that arrives together
  with a launch link that is ending playback (`app/run.rs`, `did_foreground_with_link`): it claims
  nothing, so a parked session stays parked (`Suppressed`) and the link's `Exit` retires it
  instead of racing a restore. The per-state table is on `ForegroundLifecycle::step`; add no
  event outside `ForegroundEvent`.
  Preserve the suspend/reload pairing if you touch playback.

## Verifying playback changes

**Start with `make check`** — the host unit suite (`cargo test --lib`, ~28 s for 3,639 tests,
measured 2026-09-17; do not re-quote that number, `time` it) covers a real
slice of this pipeline's pure logic: `ff.rs`'s `nal_end` bounds guard and AVCC→Annex-B conversion,
the AVIO abort guards (a seek after teardown must not open a second connection — graded on an accept
count), `stream.rs`'s socket lifecycle, `route/plan.rs`'s direct-play-vs-transcode selection, and
`task.rs`'s `MainThread` token being genuinely `!Send`. Cheap enough that there is no reason to skip
it before a deploy.

But there is **no host *runtime*** — nothing above decodes a frame or touches Starfish/ACB (`ff.rs`
once gated `#[link]` directives out of `cfg(test)` to keep the pure logic host-testable; with
everything on `dynlib!` those are gone, so a test that actually calls FFmpeg now fails by taking
`dlopen`'s `None` branch on Darwin rather than by failing to link), and the host is Darwin while the TV is
Linux, which is why `tools/sockprobe.c` exists. So anything about *playback behaviour* is only
observable on device: deploy and read `/tmp/plxnative-events.log` (feed stats, bind steps,
seek/rebase, `RECEIVE_GOOD_VIDEO`). The `tests/` harness drives real playback per case — see the root
`docs/agent-reference.md` testing section (run as GUEST by default; never run two harness jobs at once).
