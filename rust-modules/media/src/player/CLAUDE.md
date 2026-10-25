# player/ — the buffer-feed video engine

This is the in-process **buffer-feed** playback engine (was `src/playback.c`): it pulls the part
stream, demuxes it to access units, and `Feed()`s them to LG's StarfishMediaAPIs while `libAcbAPI`
binds the decoded sink to the hardware video plane. The Starfish/ACB calls cross into C at the seam
`src/starfish.c` (outside this dir — edits there almost always pair with edits here); the **ABI and
bind-order gotchas for that seam live in THIS file**, below. `docs/agent-reference.md` carries only the
one-paragraph playback summary.

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
  `settle_release`: on the first frame after the Unload returns (`run.rs` → `poll_release`), or
  at once, blocking, from `start_bufferfeed_inner`, `pa.mt()`, `pa.split()` and `shutdown`, so
  nothing reaches the seam or builds a new object while the old one is still inside Unload. While
  `Releasing`, only that epoch's `UnloadCompleted` callback is admitted (it is D1's evidence) and no
  native session can begin. Reloads, suspend and the decoder-release park (`for_reload == true`)
  keep the synchronous release. The log reads `native lifecycle: off-frame Unload epoch=N released
  X ms after the stop`, with `(a caller waited for it)` when a blocking caller had to wait.
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
  emitted only after the boot-time configd probe definitely confirms hardware support. The route
  freezes that capability + presentation decision, and every Load/reload/recovery consumes the
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
  drops: the sink does not count a frame it never presented as dropped.
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
  refused FRESH start sets `ps.native_start_refused`, which `player::state` derives `Error` from
  (cause `load_timeout`, Try Again offered) because no Engine exists to publish it; the first start that is not refused (`start_bufferfeed_for`, which every reload shares) clears it. A Load that has not
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
    (`route::dispatch_rollback_rebase`) and the HLS fallback of an Auto Original that never opened
    (`route::dispatch_unopened_auto_hls`). They hold nothing (the Engine that failed is not
    playing) and the viewer keeps the connecting spinner (`pump::hold_connecting_state`). The
    landing is drained AHEAD of the failure branches (`route::take_ready_recovery_flight`, then
    `pump::land_recovery_flight`: reload onto the route, or fail the Engine), because the
    branches' failure flags stay up for the whole flight and `fail_current_engine` would end it;
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
  (`player::to_sink_ns`, inverted on the presented-frame callback by `from_sink_ns`).
  A speed change is a seek to the viewer's own position marked `rate_reload`, and while the speed is not
  1x every direct-play seek reloads in every mode (`pump::seek_must_reload`). `play_rate_permille` is the
  viewer's choice and persists across reloads; `applied_rate_permille` is the current pipeline's and is
  reset by `reset_session`. The feed throttle's run-on reads the applied one (`FeedPace::reference_at`).
- **Seeks are in-place**, in one of three per-session modes (`player::SeekMode`, chosen by
  `engine::select_seek_mode` in `start_bufferfeed`, the `/tmp/plxnative-seekmode` dev trigger
  `native|segment|reload` wins): the demuxer is `av_seek_frame`d to the target, then on the first
  post-seek keyframe `feed_stream` re-anchors the pipeline — no reload/decoder re-init.
  `Segment` (webOS 4.x) is Kodi-style: pump `flush()`, then `setTimeToDecode` + `sendSegmentEvent`
  through decompile-derived private offsets. `NativeSeek` (`VP_EXPORTED`, webOS 5+; issue #568) is
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
  (old-ABI 3.x, and the fallback of both) is a fresh `Load` per seek. Otherwise the mode is
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
