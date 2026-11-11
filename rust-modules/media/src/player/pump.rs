//! Common route and recovery pump. The physical feed tick in `feed_pump` binds the plane,
//! feeds AUs, and returns the stage and EOS facts used here for common presentation policy.
#[cfg(all(test, feature = "hostsim"))]
use plx_play_feed::shared::FEED_SHARED;
#[cfg(all(test, feature = "hostsim"))]
use plx_play_feed::feed_state::Engine;
#[cfg(all(test, feature = "hostsim"))]
use plx_playback_contract::feed::Stage;
use super::SHARED;
use plx_playback_contract::feed::SeekMode;
#[cfg(all(test, feature = "hostsim"))]
#[cfg(all(test, feature = "hostsim"))]
use plx_base::task::MainThread;
use std::sync::atomic::Ordering::Acquire;
#[cfg(all(test, feature = "hostsim"))]
use std::sync::atomic::Ordering::Release;
#[cfg(all(test, feature = "hostsim"))]
use std::time::Duration;

/// Attach the physical Load gate's timeout to the current common trace: the feed ran the Load
/// budget check ([`plx_play_feed::feed_pump::NATIVE_LOAD_BUDGET`]), this only attributes its verdict.
fn note_load_timeout(timeout: plx_playback_contract::feed::LoadTimeout) {
    let class = match timeout {
        plx_playback_contract::feed::LoadTimeout::Elapsed(elapsed) =>
            super::report::LoadElapsedClass::from_ms(elapsed.as_millis() as i64),
        plx_playback_contract::feed::LoadTimeout::LostEpoch => super::report::LoadElapsedClass::Over20s,
    };
    super::report::note_load_gate_for(crate::route::playback_trace_generation(), class);
}

/// This session's direct-play seek mode, as the feed's last sample carries it.
fn seek_mode(ps: &crate::route::PlaybackSession) -> SeekMode { ps.readout().raw().seek_mode }

/// Publish the one value the HUD renders from: the session's readout holds it, and this is the
/// pump's event for the states it settles outside the per-frame ladder (the ladder itself is the
/// readout's `Presented` derivation). It runs on every path out of `pump` (including the early
/// returns) so the state can never go stale behind a spinner that never resolves.
fn set_state(ps: &mut crate::route::PlaybackSession, s: plx_playback_contract::readout::PlaybackState) {
    ps.step_readout(super::readout::ReadoutEvent::StateDerived(s));
}

/// How many seek requests this pump seek MERGED — i.e. requests that arrived while an earlier
/// seek was still resolving and so never got a seek of their own. Reset as it's read, so each
/// logged seek reports only the requests it consumed.
///
/// The count is what makes coalescing observable. `seek_to_ns` only ever holds the newest target,
/// so a burst of six taps and a single tap leave identical state behind; and the burst is applied
/// by whichever of two paths wins a race with the stuck-watchdog, which log different lines.
/// Counting either line therefore measures PMS latency, not merging — `coalesced=` measures
/// merging directly, and both paths report it.
/// Whether a direct-play seek in `mode` is a full fresh `Load` (`engine::reload_at`) rather than an
/// in-place one. Only `Reload` is: `Segment` and `NativeSeek` (LG's public `Seek`, issue #568) both
/// seek in place.
pub(super) fn seek_reloads(mode: SeekMode) -> bool {
    mode == SeekMode::Reload
}

/// How long a speed session may wait after loadCompleted for the pipeline's audio report before the
/// safety net assumes the PCM was not taken. On the 3.9.3 set the report arrives before
/// loadCompleted itself.
const SPEED_AUDIO_DEADLINE_MS: u32 = 8_000;

/// Whether THIS direct-play seek must be a full fresh `Load`: always in a reload mode, and in an
/// in-place mode whenever playback speed is involved. A speed change rides a seek to the current
/// position (`player::set_play_rate`, the one-shot `rate_reload`), and while the speed is not 1x
/// every seek reloads too: a speed is fixed per Load (its PCM declaration, its time-stretch and its
/// timeline all start with the session), and an in-place seek re-anchors the segment at content
/// time.
pub(super) fn seek_must_reload(mode: SeekMode, rate_permille: i32, rate_reload: bool) -> bool {
    seek_reloads(mode) || rate_reload || rate_permille != super::RATE_NORMAL
}

#[cfg(test)]
use plx_play_feed::feed_pump::prime_before_play;
#[cfg(test)]
use plx_play_feed::feed_pump::{
    hls_buffered_ms, native_clock_silence_ms, native_clock_stopped,
    rebuffer_request_is_current, should_begin_hls_rebuffer,
    CLOCK_WATCH_SINCE_MS, NATIVE_CLOCK_STOPPED_MS,
};

/// Mirror Engine-confined observations into `Shared` for diagnostics and handled-error context.
///
/// The read-out cannot reach the `Engine` itself: the draw runs inside a frame where the pump
/// holds the adapter's `&mut`, and it holds no `&mut PlayerAdapter` of its own (before phase 9 the
/// slot was a `static mut` handing out `&'static mut`, so the same call was possible and was
/// instant UB). This is the only bridge, it is one-way, and nothing in the playback state machine
/// may read these fields back.
///
/// The stage is a cheap scalar and is always published so an opted-in terminal report does not
/// depend on whether Stats for Nerds happened to be open. `aq_bytes` takes each queue's pthread
/// mutex, which is why the rest is sampled HERE, once per visible diagnostics tick, and never from
/// a draw.
/// Last `frames` we saw, so a CHANGE can be stamped. A decrease counts: `frames` is seek-scoped
/// and the pump zeroes it applying a seek, which is motion, not a freeze.
static LAST_FRAMES: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);

/// The failed half of the deferred Original recovery: put the old route back and say where to
/// resume it, in nanoseconds. `None` means there was no recovery in flight, and the caller's
/// failure means what it always did.
///
/// The three failure gates below all route through here rather than one of them, because the three
/// ways a source can refuse to open are not distinguishable from the outside and all three cost
/// the same thing: `demux_io_failed` is the transfer dying, `demux_failed` with no frames is the
/// open being rejected, and `load_failed` is the pipeline refusing the payload the source implied.
/// The device case was the first; a 404 or a codec the set will not take are the other two.
///
/// The first half of the way back from a failed Original open: take the rollback and say why the
/// Original was refused. `None` means no recovery was pending. What is left is the rebase of the
/// restored route — a flight either way: on the pump's frames ([`drive_open_recovery`]), or
/// Engine-less for the two callers that have no Engine for a pump to drive
/// ([`begin_engineless_rollback`]).
fn take_failed_original_rollback(ps: &mut crate::route::PlaybackSession, status: i32) -> Option<crate::route::OriginalRollback> {
    let rollback = crate::route::rollback_original_recovery(ps)?;
    if status >= 400 {
        super::note_original_failure(ps, super::ABR_FAILURE_ORIGINAL_HTTP, status);
    } else {
        // HTTP 200 followed by demux/Load failure is meaningfully different from no response.
        super::note_original_failure(ps, super::ABR_FAILURE_ORIGINAL_OPEN, status);
    }
    Some(rollback)
}

/// What [`recover_failed_foreground_original`] / [`land_engineless_rollback`] settled for the
/// foreground restore: the way back from an Original trial whose Load failed before an Engine
/// existed, so there is no Engine for the pump to recover.
pub enum ForegroundOriginalRecovery {
    NotOriginal,
    /// The rollback's rebase of the restored HLS route is a flight no Engine waits on
    /// ([`land_engineless_rollback`] delivers it).
    Pending,
    Tracking(crate::route::RouteStartAttempt),
    /// PMS preparation succeeded and the HLS candidate remains retryable, but native Engine
    /// construction failed before an asynchronous Load could be observed.
    RetryPrepared,
    /// Rollback consumed the Original snapshot but could not rebuild/rebase a truthful HLS route.
    Terminal,
}

/// Start the rollback of an Original trial whose Load failed while its Engine was being built (no
/// Engine exists for [`pump`] to inspect): take the typed Original -> restored-HLS edge and
/// dispatch its rebase as a flight no Engine waits on. The landing is
/// [`land_engineless_rollback`]'s.
fn begin_engineless_rollback(ps: &mut crate::route::PlaybackSession, status: i32) -> ForegroundOriginalRecovery {
    let Some(rollback) = take_failed_original_rollback(ps, status) else {
        return ForegroundOriginalRecovery::NotOriginal;
    };
    // The held rollback resource is still alive, but its old start URL begins at the boundary
    // where that worker was created. Register a fresh physical HLS session at the recovery
    // position before reloading it; reopening the saved URL pairs a new display base with old media
    // and makes the picture jump backwards while the clock claims it did not.
    match crate::route::dispatch_engineless_rollback_rebase(ps, rollback.offset_ns) {
        crate::route::RecoveryDispatch::Flying { .. } => ForegroundOriginalRecovery::Pending,
        crate::route::RecoveryDispatch::Refused => {
            super::log(crate::route::ROLLBACK_REBASE_REFUSED);
            set_state(ps, plx_playback_contract::readout::PlaybackState::Error);
            ForegroundOriginalRecovery::Terminal
        }
    }
}

/// The foreground restore's synchronous Load failure inside an Original trial. No HTTP request
/// necessarily happened: do not inherit the previous Engine's sticky status.
pub fn recover_failed_foreground_original(ps: &mut crate::route::PlaybackSession) -> ForegroundOriginalRecovery {
    begin_engineless_rollback(ps, 0)
}

/// What draining an Engine-less rollback's landing found.
pub enum RollbackLanding {
    /// The worker has not landed, or the outstanding flight is not a rollback's.
    Waiting,
    /// No Engine-less flight is outstanding: it ended (a Back, a suspend, a newer request) and
    /// whoever ended it owns what happens next. A late landing has been reaped.
    Stale,
    Landed(ForegroundOriginalRecovery),
}

/// Drain the landing of a rollback no Engine waits on, once a frame while
/// [`crate::route::engineless_flight_outstanding`]: reload onto the restored HLS route at the
/// recovery position, or — the rebase was refused — fail the start. Reaps a stale flight's late landing first, since no pump will.
pub fn land_engineless_rollback(ps: &mut crate::route::PlaybackSession, pa: &mut super::adapter::PlayerAdapter) -> RollbackLanding {
    if super::accept_server_stop(ps) {
        return RollbackLanding::Landed(ForegroundOriginalRecovery::Terminal);
    }
    crate::route::discard_stale_flight_landing();
    if !crate::route::engineless_flight_outstanding() {
        return RollbackLanding::Stale;
    }
    if crate::route::resume_flight_outstanding() {
        return RollbackLanding::Waiting; // a resume's flight: `land_resume` drains it
    }
    let Some(landing) = crate::route::take_ready_recovery_flight(ps) else {
        return RollbackLanding::Waiting;
    };
    RollbackLanding::Landed(match landing.verdict {
        crate::route::RecoveryVerdict::Install => match super::engine::reload_transcode_tracked(ps, pa, landing.offset_ns) {
            super::engine::BufferfeedStartOutcome::Launched(attempt) => ForegroundOriginalRecovery::Tracking(attempt),
            // A reload's teardown released synchronously, so nothing defers it; if one ever did, the
            // queued transaction is the same one the retry names.
            super::engine::BufferfeedStartOutcome::Failed | super::engine::BufferfeedStartOutcome::Deferred => {
                ForegroundOriginalRecovery::RetryPrepared
            }
            super::engine::BufferfeedStartOutcome::AlreadyRunning => {
                set_state(ps, plx_playback_contract::readout::PlaybackState::Error);
                ForegroundOriginalRecovery::Terminal
            }
        },
        // `install_recovery_landing` already said `ROLLBACK_REBASE_REFUSED`.
        crate::route::RecoveryVerdict::Refused => {
            set_state(ps, plx_playback_contract::readout::PlaybackState::Error);
            ForegroundOriginalRecovery::Terminal
        }
    })
}

/// The app loop's once-a-frame drain for a rollback NO foreground machine owns (the pump's
/// `start_original_trial_reload` found its Load failing synchronously): a reload that does not
/// start is the failure read-out.
pub fn drain_engineless_rollback(ps: &mut crate::route::PlaybackSession, pa: &mut super::adapter::PlayerAdapter) {
    if let RollbackLanding::Landed(ForegroundOriginalRecovery::RetryPrepared) = land_engineless_rollback(ps, pa) {
        super::log("abr: HLS rollback could not start (StartFailed)");
        set_state(ps, plx_playback_contract::readout::PlaybackState::Error);
    }
}

/// A preview is direct-play only. A failed original open must not become an HLS transcode, and
/// it must not land on the player error read-out. On the host seam with no clock sink, `sf_load`
/// returns 0 because there is no video path. That is not an admitted Starfish failure, so it
/// must not open the preview breaker.
fn finish_preview_open_failure(
    ps: &mut crate::route::PlaybackSession,
    pa: &mut super::adapter::PlayerAdapter,
) {
    // Already retired, and its Load has not returned yet: `preview::after_pump` finishes it.
    // The failure flag stays set until then, so this arm is reached every frame in between.
    if crate::player::preview::abandoning() {
        return;
    }
    let seam_absent = cfg!(feature = "hostsim") && !plx_base::devtrig::flag("clocksink");
    if seam_absent {
        crate::player::preview::note_admission_refused();
    } else {
        crate::player::preview::note_failed(601);
    }
    crate::player::preview::retire(ps, pa, "open failed");
}

/// What the pump's failure branches got from [`recover_failed_open`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OpenRecoveryRun {
    /// A flight owns the replacement route; [`land_recovery_flight`] reloads onto it, or fails the
    /// Engine, when it lands. The frames in between wait ([`recovery_flight_outstanding`]).
    Flying,
    /// The ladder's app-decode rung (the very last one for a surround source): the same direct play,
    /// reloaded at `offset_ns` with the app decoding its audio ([`decode_in_app`]). No flight: the
    /// caller reloads.
    DecodeInApp { offset_ns: i64 },
    /// Nothing to recover with: the Engine failed for good and the viewer has the read-out.
    Failed,
    /// The ladder asked for nothing; the frame goes on.
    Quiet,
}

/// What the ladder decides on, read at the moment of a failure ([`super::recovery::OpenFacts`]).
/// `native_audio` is what the caller's failure names ([`native_audio_fact`]); the branches which
/// are not a pipeline refusal pass `None`.
fn open_facts(ps: &crate::route::PlaybackSession, native_audio: Option<plx_platform::devcaps::Facet>) -> super::recovery::OpenFacts {
    super::recovery::OpenFacts {
        pending_original: crate::route::original_recovery_pending(),
        auto_original: crate::route::auto_original_watch(ps).is_some(),
        no_frames: ps.readout().raw().frames == 0,
        native_audio,
        forced: crate::route::forced_direct_play(ps),
        direct: !crate::route::is_transcoding(ps),
        audio: crate::route::stream_audio(ps),
        video: crate::route::stream_video(ps),
    }
}

/// Whether a failed Load is the foreground lifecycle's to settle rather than the ladder's. The rule
/// is who observes the refusal: [`super::recovery::OpenRecovery`] owns a refusal the pump observes
/// AFTER a Load call returned (`type=18` once the reducer read `Started`, a Load budget, a stall, a
/// source failure); the lifecycle keeps the Load call's own synchronous result (`Failed`), which it
/// reads through the same reducer and settles as #1018's retry. Both facts are the driver's, read
/// before the machine is stepped, so the machine never sees this refusal.
fn settled_by_foreground(foreground_follows: bool, start: crate::route::RouteStartStatus) -> bool {
    foreground_follows && start == crate::route::RouteStartStatus::Failed
}

/// The pipeline's own account of the failed Load, from the flags its callbacks latched. The flag
/// and the code that explains it are one sample (the readout's last ingestion, which the feed loads
/// flag first), so a code a worker published before its flag is never read as 0.
fn load_refusal(ps: &crate::route::PlaybackSession) -> super::recovery::LoadRefusal {
    let raw = ps.readout().raw();
    if raw.flags.load_timed_out {
        super::recovery::LoadRefusal::TimedOut
    } else {
        super::recovery::LoadRefusal::Refused { num: raw.cb_err_num }
    }
}

/// The native audio a refusal just named: the flag `sf_on_event` (`type=18 num=200` while audio was
/// fed natively) or the stall watchdog raised, and the audio the demuxer is feeding. Peeks the flag,
/// which [`confirm_refusal`] consumes when the ladder asks for the write (the facet itself travels
/// on the effect); a flag raised with nothing fed natively is dropped here, as it always was.
fn native_audio_fact(ps: &mut crate::route::PlaybackSession) -> Option<plx_platform::devcaps::Facet> {
    if !ps.readout().native_audio_refused() {
        return None;
    }
    let fed = refused_native_audio(ps);
    if fed.is_none() {
        ps.step_readout(super::readout::ReadoutEvent::Taken {
            edge: super::readout::Edge::NativeAudioRefused,
        });
    }
    fed
}

/// Run `first` through the request's failed-open ladder ([`super::recovery::OpenRecovery`]) and
/// execute what it asks for. **One event in a single slot**: a step's effects run in order, and the
/// one answer an effect gives (a flight that could not start) is the next event, until a step asks
/// for nothing more. The learner's write ([`confirm_refusal`]) is fire-and-forget: nothing answers it.
/// `why` words its log line.
///
/// Pending HLS rollback has priority; a failed rollback is terminal rather than silently starting a
/// third transaction on route state whose encoder restore already failed. A pipeline refusal of a
/// stream the attempt can still do without (`recovery::suspect`: the audio outside the codecs every
/// set decodes, else a numbered refusal's source video) rebuilds the same route converted, once per
/// request: a refused audio keeps the route as planned (an Original remux still copies the video),
/// a refused video is an encode under the 1080p ceiling (`VideoLane`). A cold Auto Original falls
/// back to HLS after an audio conversion, and takes the HLS contingency at once for a refused video
/// (the HLS rung is already its video re-encode), the refused raster out of the HLS attempt; a
/// manual pick whose refusal follows its facet flight (or whose pipeline refusal named no suspect
/// stream) takes the both-facets rung once, an encode with no video copy under the attempt that lacks
/// the audio and the video raster the refusal named (`VideoLane::Both`), and the refusal after that
/// reads out. Each recovery is a flight: the replacement route's PMS half runs on a worker while the viewer keeps
/// the connecting spinner they had.
fn drive_open_recovery(ps: &mut crate::route::PlaybackSession, first: super::recovery::RecoveryEvent, why: &str) -> OpenRecoveryRun {
    use super::recovery::{Evidence, RecoveryEffect, RecoveryEvent};
    let mut run = OpenRecoveryRun::Quiet;
    let mut ev = first;
    loop {
        let mut fx = Vec::new();
        ps.open_recovery().step(&ev, &mut fx);
        let mut answer = None;
        for effect in fx {
            let dispatched = match effect {
                RecoveryEffect::ConfirmRefusal { facet, evidence } => {
                    let record: fn(&plx_platform::devcaps::Facet) -> bool = match evidence {
                        Evidence::Named => plx_plex::plex::session::record_named_refusal,
                        Evidence::Proof => plx_plex::plex::session::record_refusal_proof,
                    };
                    confirm_refusal(ps, &facet, evidence, why, record);
                    continue;
                }
                RecoveryEffect::Rollback => {
                    // Capture before the reload clears the engine-scoped HTTP mirror. This sticky
                    // pair is the reason a successful HLS rollback can still explain on screen why
                    // Original was refused.
                    let status = ps.readout().raw().http_status;
                    match take_failed_original_rollback(ps, status) {
                        None => crate::route::RecoveryDispatch::Refused,
                        Some(rollback) => {
                            let dispatched = crate::route::dispatch_rollback_rebase(ps, rollback.offset_ns);
                            if dispatched == crate::route::RecoveryDispatch::Refused {
                                super::log(crate::route::ROLLBACK_REBASE_REFUSED);
                            }
                            dispatched
                        }
                    }
                }
                RecoveryEffect::ConvertFacet { facet, flight } => {
                    // The machine narrowed the attempt to `start` less this facet when it entered the
                    // rung; the dispatch plans from `attempt()`, not from anything read here.
                    super::log(&format!(
                        "open recovery: converting {facet:?} (flight {flight}/{})",
                        super::recovery::MAX_RECOVERY_FLIGHTS
                    ));
                    let secs = (ps.readout().raw().playpos_ns / 1_000_000_000).max(0);
                    // A refused video is converted by an encode that withdraws the video copy; a
                    // refused audio keeps the route as planned (an Original remux still copies the
                    // video).
                    let lane = match facet {
                        plx_platform::devcaps::Facet::Audio { .. } => crate::route::VideoLane::AsPlanned,
                        plx_platform::devcaps::Facet::Video { .. } => crate::route::VideoLane::Encode,
                    };
                    crate::route::dispatch_refusal_conversion(ps, secs, lane)
                }
                RecoveryEffect::ConvertBoth { flight } => {
                    // The manual both-facets rung: the audio converted AND the video encoded. The
                    // attempt (narrowed by the machine) already lacks what this refusal named.
                    super::log(&format!(
                        "open recovery: converting both facets (flight {flight}/{})",
                        super::recovery::MAX_RECOVERY_FLIGHTS
                    ));
                    let secs = (ps.readout().raw().playpos_ns / 1_000_000_000).max(0);
                    crate::route::dispatch_refusal_conversion(ps, secs, crate::route::VideoLane::Both)
                }
                RecoveryEffect::StartAutoHls => {
                    let status = ps.readout().raw().http_status;
                    if status >= 400 {
                        super::note_original_failure(ps, super::ABR_FAILURE_ORIGINAL_HTTP, status);
                    } else {
                        super::note_original_failure(ps, super::ABR_FAILURE_ORIGINAL_OPEN, status);
                    }
                    let secs = (ps.readout().raw().playpos_ns / 1_000_000_000).max(0);
                    crate::route::dispatch_unopened_auto_hls(ps, secs)
                }
                RecoveryEffect::DecodeInApp { audio, refusal } => {
                    if let plx_platform::devcaps::Facet::Audio { codec, .. } = &audio {
                        super::log(&format!("open recovery: decoding {codec} in the app (stereo) after {refusal}"));
                    }
                    run = OpenRecoveryRun::DecodeInApp { offset_ns: ps.readout().raw().playpos_ns };
                    continue;
                }
                RecoveryEffect::NameRefusedAudio { codec } => {
                    crate::route::name_refused_forced_audio(ps, &codec);
                    continue;
                }
                RecoveryEffect::ShowError { refusal } => {
                    super::log(&format!("open recovery: no rung left for {refusal}; failing the Engine"));
                    fail_engine_for_good(ps);
                    run = OpenRecoveryRun::Failed;
                    continue;
                }
                RecoveryEffect::ReportRecovered(recovered) => {
                    // One event per rescued request, under the error-report consent (the report
                    // path checks it); nothing answers.
                    super::log(&format!(
                        "open recovery: the refusal was recovered on the {:?} rung ({})",
                        recovered.rung, recovered.refusal
                    ));
                    super::report::refusal_recovered(&recovered);
                    continue;
                }
            };
            match dispatched {
                crate::route::RecoveryDispatch::Flying { .. } => run = OpenRecoveryRun::Flying,
                crate::route::RecoveryDispatch::Refused => answer = Some(RecoveryEvent::FlightRefused),
            }
        }
        match answer {
            Some(next) => ev = next,
            None => return run,
        }
    }
}

/// A failed open: run the ladder, and for a recovery that flies keep the connecting spinner the
/// failed open left. The failure branches' shared answer, always the frame's last act.
///
/// **A refusal is never silenced.** The machine answers [`OpenRecoveryRun::Quiet`] only when it has
/// no question for the refusal (a state outstanding with a flight or the read-out, which the next
/// Load of the request now takes out of: `RecoveryEvent::LoadStarted`), and the failed flags stay
/// up for every frame the Engine lives: a frame which asked for nothing would leave the viewer on
/// the spinner for good. The Engine fails, as it did before the machine; in the state the read-out
/// is already up this re-asserts it (idempotent and silent), once a frame until the Engine is released.
///
/// The one answer it hands back is [`OpenRecoveryRun::DecodeInApp`]: the reload needs the adapter,
/// which the caller still holds the Engine's borrow of, so the caller runs [`decode_in_app`].
fn recover_failed_open(
    ps: &mut crate::route::PlaybackSession,
    loading: bool,
    refusal: super::recovery::LoadRefusal,
    native_audio: Option<plx_platform::devcaps::Facet>,
    why: &str,
) -> OpenRecoveryRun {
    let facts = open_facts(ps, native_audio);
    let run = drive_open_recovery(ps, super::recovery::RecoveryEvent::Refused { refusal, facts }, why);
    match run {
        OpenRecoveryRun::Flying => hold_connecting_state(ps, loading),
        OpenRecoveryRun::Failed | OpenRecoveryRun::DecodeInApp { .. } => {}
        OpenRecoveryRun::Quiet => fail_engine_for_good(ps),
    }
    run
}

/// The ladder's app-decode rung ([`super::recovery::RecoveryEffect::DecodeInApp`]; the very last rung
/// for a source of more than two channels, which this mixes down): reload the SAME direct
/// play at `offset_ns`. The machine has already put the refused codec in force in the attempt, so the
/// fresh Load declares stereo 48 kHz PCM and the demuxer decodes the audio itself; no server is asked.
/// A route that is a server session (the machine only picks a direct play) fails the Engine instead.
fn decode_in_app(ps: &mut crate::route::PlaybackSession, pa: &mut super::adapter::PlayerAdapter, offset_ns: i64) {
    if crate::route::is_transcoding(ps) {
        return fail_engine_for_good(ps);
    }
    // This recovery rung changes the physical recipe while preserving the original URL.
    // The attempt's narrowed caps now declare PCM; spend the fixed app-audio candidate before
    // the reload instead of reopening the refused native-audio recipe.
    if !ps.qualify_installed_feed_offer(crate::route::CandidatePurpose::NewCandidate) {
        return fail_engine_for_good(ps);
    }
    super::report::note_refusal_recovery_delivery(ps);
    let _ = settle_reload(super::engine::reload_at(ps, pa, offset_ns), "app decode after a refused open", ps);
}

/// The native audio the demuxer is feeding, as the capability facet the pipeline's refusal of it
/// removes from an attempt (`Caps::without`). `None` when nothing is fed natively.
fn refused_native_audio(ps: &crate::route::PlaybackSession) -> Option<plx_platform::devcaps::Facet> {
    let fed = ps.readout().raw().native_audio?;
    Some(plx_platform::devcaps::Facet::Audio { codec: fed.codec.into(), channels: fed.channels })
}

/// The machine asked for `facet` to be recorded as refused on this firmware (`record`, which is
/// `session::record_named_refusal` or `record_refusal_proof` in the player and a recorder in the
/// tests). A [`Named`](super::recovery::Evidence::Named) write is the pipeline's own refusal
/// (`type=18 num=200` with the audio fed natively, or the stall): it consumes the flag that raised it,
/// so one raise writes once. A [`Proof`](super::recovery::Evidence::Proof) write is a facet-clean
/// rescue at the first picture and touches no flag. True when `record` queued a write. The answer selects nothing: the machine's facet rung is chosen by
/// [`super::recovery::suspect`] and its loop guard, so a combination already stored, and so already
/// absent from the attempt, gets no second conversion; the answer only words nothing today.
fn confirm_refusal(
    ps: &mut crate::route::PlaybackSession,
    facet: &plx_platform::devcaps::Facet,
    evidence: super::recovery::Evidence,
    why: &str,
    record: impl FnOnce(&plx_platform::devcaps::Facet) -> bool,
) -> bool {
    match evidence {
        super::recovery::Evidence::Named => {
            if !ps.readout().native_audio_refused() {
                return false;
            }
            ps.step_readout(super::readout::ReadoutEvent::Taken {
                edge: super::readout::Edge::NativeAudioRefused,
            });
            super::log(&format!("native audio: the pipeline refused {facet:?} ({why})"));
        }
        super::recovery::Evidence::Proof => {
            super::log(&format!("open recovery: {facet:?} proved refused (flight landed, video copied)"));
        }
    }
    record(facet)
}

/// Withdraw the route's Dolby Vision declaration for `w` and answer where the reload goes. `None`: the
/// route refused (it no longer declares what it can take out, a race the arming did not see), the
/// Feed's guard stands down and the held landing feeds.
///
/// The target is read NOW, not when the withdrawal was raised: the newest coalesced tap if one is
/// pending (as the seek stuck-watchdog adopts it, `commit_user_seek` included), else, for an in-place
/// landing, the seek's own target (as a refused native `Seek` re-posts it), else the target the
/// Engine was armed with (a resume), else the landed keyframe (a plain start).
fn withdraw_dolby_vision(
    ps: &mut crate::route::PlaybackSession,
    pa: &mut super::adapter::PlayerAdapter,
    w: &plx_playback_contract::feed::DvWithdrawal,
) -> Option<i64> {
    if !crate::route::withdraw_dv_presentation(ps, &w.why) {
        super::log("dv: the route no longer declares a Dolby Vision node it can withdraw; nothing changed");
        pa.feed_command(plx_playback_contract::feed::FeedCommand::DvStandDown);
        return None;
    }
    let target = if let Some(tap) = super::take_pending_seek(ps, pa) {
        crate::route::commit_user_seek();
        tap.target_ns
    } else {
        // The feed's in-place target rides the sample (`FeedRaw::seek_target_ns`).
        let in_place = ps.readout().raw().seek_target_ns.unwrap_or(-1);
        if w.in_place && in_place >= 0 { in_place } else { w.armed_target.unwrap_or(w.kf_pts) }
    }
    .max(0);
    super::log(&format!(
        "dv: withdrawn for this play: {}; reload at {:.3}s without DolbyHdrInfo, the base layer plays",
        w.why,
        target as f64 / 1e9,
    ));
    Some(target)
}

/// The Engine fails for good: any flight ends, the viewer gets the read-out, and the Engine's
/// pipeline is released next frame ([`super::route_start::release_failed_engine`]).
fn fail_engine_for_good(ps: &mut crate::route::PlaybackSession) {
    crate::route::fail_current_engine();
    set_state(ps, plx_playback_contract::readout::PlaybackState::Error);
    super::engine::arm_failed_engine_release();
}

/// What the viewer sees while a recovery flies: the connecting spinner the failed open left, not a
/// frozen frame and not the failure read-out. Mirrors the ladder at the foot of [`pump`] for a
/// stream that has decoded nothing.
fn hold_connecting_state(ps: &mut crate::route::PlaybackSession, loading: bool) {
    set_state(ps, if loading {
        plx_playback_contract::readout::PlaybackState::Connecting
    } else {
        plx_playback_contract::readout::PlaybackState::Buffering
    });
}

/// A recovery flight landed: reload onto the replacement route at the recovery position, or — it
/// was refused — hand `FlightRefused` to the machine, whose next rung is the HLS contingency (a cold
/// Auto Original's refused facet conversion) or the read-out. Nothing fails here itself, except an
/// answer the machine does not give: that too is the Engine failing, never silence.
fn land_recovery_flight(
    ps: &mut crate::route::PlaybackSession,
    pa: &mut super::adapter::PlayerAdapter,
    landing: crate::route::RecoveryLanding,
) {
    match landing.verdict {
        crate::route::RecoveryVerdict::Install => {
            let _ = drive_open_recovery(ps, super::recovery::RecoveryEvent::FlightLanded { lanes: landing.lanes }, "");
            let _ = settle_reload(
                super::engine::reload_transcode(ps, pa, landing.offset_ns),
                "recovery after failed source open", ps,
            );
        }
        crate::route::RecoveryVerdict::Refused => {
            match drive_open_recovery(ps, super::recovery::RecoveryEvent::FlightRefused, "") {
                OpenRecoveryRun::DecodeInApp { offset_ns } => decode_in_app(ps, pa, offset_ns),
                OpenRecoveryRun::Quiet => fail_engine_for_good(ps),
                OpenRecoveryRun::Flying | OpenRecoveryRun::Failed => {}
            }
        }
    }
}

/// Settle a synchronous native reload effect.  A failed `start_bufferfeed` used to be only a log:
/// callers had already advanced the route reducer and then returned, leaving no Engine to publish
/// the failure and (for Original) an `OriginalTrial` which could never receive its first frame.
///
/// The session is the LAST argument: the outcome is usually the value of a reload that borrowed it.
pub(super) fn settle_reload(
    outcome: super::engine::ReloadOutcome,
    operation: &str,
    ps: &mut crate::route::PlaybackSession,
) -> bool {
    if outcome == super::engine::ReloadOutcome::Started {
        return true;
    }
    super::log(&format!(
        "route transition: {operation} failed synchronously ({outcome:?})",
    ));
    // The reload's teardown parked a Load that had already passed its budget and cleared the
    // session flags with the rest of that session's state, then the start was refused for the
    // outstanding object. Without the cause the report says `unspecified`; the cause is the
    // timeout. The teardown recorded the parked Load in the torn-down attempt's terminal facts
    // (`AttemptFacts::load_abandoned`, ingested before the session reset), and a refused start has
    // not begun a new attempt, so that view is still the reporting attempt's. The cause is stated
    // where a fresh start refused for the same Load states it (`start_bufferfeed`), the session's
    // `native_start_refused`, which `live_runtime_failure` reads as a timed-out Load.
    if outcome == super::engine::ReloadOutcome::StartFailed
        && ps.attempt_view().facts().load_abandoned == Some(true)
    {
        ps.native_start_refused = Some(crate::route::StartRefusal::LoadTimeout);
    }
    set_state(ps, plx_playback_contract::readout::PlaybackState::Error);
    false
}

/// Start the native half of an HLS→Original transaction.  The reducer already owns a complete HLS
/// rollback snapshot; a synchronous Load construction failure is the same typed rejection as an
/// HTTP/demux/Load callback failure and must cross that rollback edge immediately. There is no
/// Engine for a pump to drive the way back, so the rebase is an Engine-less flight
/// ([`begin_engineless_rollback`]): the viewer keeps the spinner, and the app loop's
/// [`drain_engineless_rollback`] reloads onto the restored route when it lands.
fn start_original_trial_reload(
    ps: &mut crate::route::PlaybackSession,
    pa: &mut super::adapter::PlayerAdapter,
    reload: crate::route::AutoOriginalReload,
    position_ns: i64,
) {
    let outcome = match reload {
        crate::route::AutoOriginalReload::Direct => super::engine::reload_at(ps, pa, position_ns),
        crate::route::AutoOriginalReload::Remux => super::engine::reload_transcode(ps, pa, position_ns),
    };
    if outcome == super::engine::ReloadOutcome::Started {
        return;
    }
    super::log(&format!(
        "abr: Original trial could not start ({outcome:?}); taking explicit rollback edge",
    ));
    // Capture before the reload clears the engine-scoped HTTP mirror. This sticky pair is the
    // reason a successful HLS rollback can still explain on screen why Original was refused.
    let status = ps.readout().raw().http_status;
    if matches!(
        begin_engineless_rollback(ps, status),
        ForegroundOriginalRecovery::NotOriginal | ForegroundOriginalRecovery::Terminal
    ) {
        set_state(ps, plx_playback_contract::readout::PlaybackState::Error);
    }
}

/// Physical half of a claimed user action whose PMS half `route` already ran (`ClaimTail`).
/// Returns whether it replaced the Engine: every arm but `Rejected` does, and the caller must
/// return immediately in that case; `Rejected` restores the previous projection and lets the
/// common route phase continue. The Engine is borrowed only after that phase ends.
fn run_claim_tail(
    ps: &mut crate::route::PlaybackSession,
    pa: &mut super::adapter::PlayerAdapter,
    action: &crate::route::ClaimedRouteAction,
    tail: crate::route::ClaimTail,
    pending_seek: i64,
    user_target: i64,
) -> bool {
    match tail {
        crate::route::ClaimTail::Retranscode => {
            retranscode_tail(ps, pa, action, pending_seek, user_target);
            true
        }
        crate::route::ClaimTail::Adaptive => {
            adaptive_tail(ps, pa, action, pending_seek, user_target);
            true
        }
        crate::route::ClaimTail::NativeAudio => {
            native_audio_tail(ps, pa, action, pending_seek, user_target);
            true
        }
        crate::route::ClaimTail::Original(reload) => {
            original_tail(ps, pa, reload, pending_seek, user_target);
            true
        }
        crate::route::ClaimTail::Rejected(msg) => {
            rejected_tail(ps, action, msg);
            false
        }
    }
}

/// At claim-landing time, decide whether the seek target this claim was built with (`pending_seek`,
/// captured when the claim was made) is still the newest one requested. The pump's own pending-seek
/// branch skips entirely while `flight_outstanding()` holds (see its own doc), so a fresher tap that
/// landed during this claim's flight sits untouched in the readout's pending target rather than being folded in —
/// committing THIS claim's stale target with `commit_user_seek` would silently discard it. Returns
/// the fresher value to re-arm into the pending target after the reload resets it (the teardown's
/// `SessionReset` and `FEED_TX.reset_for_reload`), or `None` if there is nothing to carry.
fn commit_or_carry_seek(ps: &crate::route::PlaybackSession, pending_seek: i64) -> Option<i64> {
    let current = ps.readout().pending_seek().unwrap_or(-1);
    // Anything on the mailbox that is not exactly the target this claim was built for is a press
    // the claim never saw — including the first press of the flight when NO seek was pending at
    // claim time (`pending_seek < 0`), which the reload's teardown would otherwise clear. Its reducer intent (`pending_seek_ns`) is left owed: the pump's own seek branch
    // commits it when it applies the re-armed target.
    if current >= 0 && current != pending_seek {
        return Some(current);
    }
    if pending_seek >= 0 {
        // The reload lands AT the claim's own seek: cross it in the reducer now.
        crate::route::commit_user_seek();
    }
    None
}

/// Restore a fresher seek target carried past a reload by [`commit_or_carry_seek`]. The pump's
/// ordinary pending-seek branch (no longer blocked — the claim it was waiting on just settled)
/// picks it up on the very next tick, exactly as if it had just arrived.
fn rearm_carried_seek(
    ps: &mut crate::route::PlaybackSession,
    pa: &mut super::adapter::PlayerAdapter,
    carried: Option<i64>,
) {
    if let Some(target) = carried {
        super::requeue_seek(ps, pa, target);
    }
}

/// The shared shape of the two prepared-route tails: settle the action, cross or carry the seek,
/// log the transition, run `reload`, re-arm the carried seek. `label` names the transition in the
/// log and `detail` is anything it adds after the label.
fn land_prepared_route(
    ps: &mut crate::route::PlaybackSession,
    pa: &mut super::adapter::PlayerAdapter,
    action: &crate::route::ClaimedRouteAction,
    pending_seek: i64,
    user_target: i64,
    (label, detail): (&str, &str),
    reload: impl FnOnce(&mut crate::route::PlaybackSession, &mut super::adapter::PlayerAdapter, i64) -> super::engine::ReloadOutcome,
) {
    if !crate::route::finish_route_action(ps, action, crate::route::RouteApplyResult::Prepared) {
        // A teardown or a fresh playback request superseded this action while the worker ran (see
        // `finish_route_action`'s own doc on its `bool` return) — `ps` may already belong to a
        // different item. Reloading onto it now would be exactly the corruption the mailbox-clearing
        // fix exists to prevent.
        super::log(&format!("route transition: {label} landing superseded; not reloading"));
        return;
    }
    let carried_seek = commit_or_carry_seek(ps, pending_seek);
    super::log(&format!(
        "route transition: {label}{detail} at {}s{}",
        user_target / 1_000_000_000,
        if pending_seek >= 0 { " + seek" } else { "" },
    ));
    settle_reload(reload(ps, pa, user_target), &format!("{label} reload"), ps);
    rearm_carried_seek(ps, pa, carried_seek);
}

/// A prepared transcode route: settle the action, cross the seek, reload onto it.
fn retranscode_tail(
    ps: &mut crate::route::PlaybackSession,
    pa: &mut super::adapter::PlayerAdapter,
    action: &crate::route::ClaimedRouteAction,
    pending_seek: i64,
    user_target: i64,
) {
    land_prepared_route(ps, pa, action, pending_seek, user_target, ("user retranscode", ""), super::engine::reload_transcode);
}

/// A rebuilt transcode of the same stream (an `AdaptiveReload` landing): settle the action, cross
/// the seek, reload onto it at the claim offset.
fn adaptive_tail(
    ps: &mut crate::route::PlaybackSession,
    pa: &mut super::adapter::PlayerAdapter,
    action: &crate::route::ClaimedRouteAction,
    pending_seek: i64,
    user_target: i64,
) {
    land_prepared_route(ps, pa, action, pending_seek, user_target, ("adaptive worker reload", ""), super::engine::reload_transcode);
}

/// A transcode seek's landing, run on the frame that drained it. The pump held presentation for
/// the flight ([`super::claim_hold`]); `hold` is what it owes the viewer back.
fn seek_landing_tail(
    ps: &mut crate::route::PlaybackSession,
    pa: &mut super::adapter::PlayerAdapter,
    serial: u64,
    target_ns: i64,
    verdict: crate::route::SeekVerdict,
) {
    let hold = super::claim_hold::take(serial);
    match verdict {
        crate::route::SeekVerdict::Install(_) => {
            // The route names the replacement and its start transaction is `Prepared`: this is the
            // applied media boundary, exactly where the synchronous rebuild crossed it.
            crate::route::commit_user_seek();
            super::log(&format!("route transition: transcode seek at {}s", target_ns / 1_000_000_000));
            // The viewer's own Pause (a hold that owes a Play back is not): the fresh stream must
            // be allowed to prime a frame, then be paused again by `settle_paused_seek`.
            if !hold {
                own_seek_under_pause(ps, pa, target_ns);
            }
            settle_reload(super::engine::reload_transcode(ps, pa, target_ns), "transcode seek reload", ps);
            // The reload kept the hold's pause; only now does the new stream get its Play.
            super::claim_hold::release(ps, pa, hold);
        }
        crate::route::SeekVerdict::Superseded => {
            // A newer tap is waiting in the seek target: fly at THAT, carrying the hold over.
            if let Some(newest) = super::take_pending_seek(ps, pa) {
                start_transcode_seek_flight(ps, pa, newest.target_ns, hold);
            } else {
                super::claim_hold::release(ps, pa, hold);
            }
        }
        crate::route::SeekVerdict::Refused => {
            super::claim_hold::release(ps, pa, hold);
            // Give up on THIS seek and say so, or the spinner and the frozen playhead outlive the
            // playback — see `player::abandon_seek`. The engine is untouched (no flush happened),
            // so the stream itself carries on from where it was.
            super::abandon_seek(ps, pa);
            super::log("seek(transcode): rebuild failed");
        }
    }
}

/// Start the flight for a seek on a live transcode at `target_ns` (already taken off
/// the pending slot): plan on this frame, PMS on a worker, presentation held until it lands.
/// `inherited_hold` is the restore a superseded flight's hold still owes.
fn start_transcode_seek_flight(
    ps: &mut crate::route::PlaybackSession,
    pa: &mut super::adapter::PlayerAdapter,
    target_ns: i64,
    inherited_hold: bool,
) {
    match crate::route::dispatch_transcode_seek(ps, target_ns) {
        crate::route::SeekDispatch::Flying { serial } => {
            super::claim_hold::engage_inheriting(ps, pa, serial, inherited_hold);
            super::log(&format!("seek(transcode): rebuild in flight at {}s", target_ns / 1_000_000_000));
        }
        crate::route::SeekDispatch::Busy => {
            // Another transaction holds the reducer for a frame or two: the tap stays a tap.
            super::requeue_seek(ps, pa, target_ns);
            super::claim_hold::release(ps, pa, inherited_hold);
        }
        crate::route::SeekDispatch::Refused => {
            super::claim_hold::release(ps, pa, inherited_hold);
            super::abandon_seek(ps, pa);
            super::log("seek(transcode): rebuild failed");
        }
    }
}

/// A claimed action that owes a displaced pick its reload stages the pick (route), and the feed's copy
/// follows at once: the session's audio choice is what the demuxer is told to feed.
fn honour_displaced_pick(
    ps: &mut crate::route::PlaybackSession,
    pa: &mut super::adapter::PlayerAdapter,
    displaced_pick: bool,
    arm: &str,
) {
    let idx = ps.readout().audio_choice();
    crate::route::honour_displaced_pick(ps, displaced_pick, idx, arm);
    super::publish_audio_choice(ps, pa);
}

/// A staged native audio switch (the session's audio choice + payload codec): settle, reload direct.
fn native_audio_tail(
    ps: &mut crate::route::PlaybackSession,
    pa: &mut super::adapter::PlayerAdapter,
    action: &crate::route::ClaimedRouteAction,
    pending_seek: i64,
    user_target: i64,
) {
    let idx = ps.readout().audio_choice();
    land_prepared_route(
        ps,
        pa,
        action,
        pending_seek,
        user_target,
        ("native audio", &format!(" idx={idx}")),
        |ps, pa, target| super::engine::switch_audio_native(ps, pa, idx, target),
    );
}

/// A staged Original trial: its PendingOriginal already owns the phase, so there is no
/// `finish_route_action` here; the reload lands on the requested position. A seek pressed during
/// the recovery's flight is carried past the reload ([`commit_or_carry_seek`]), not dropped by it.
fn original_tail(
    ps: &mut crate::route::PlaybackSession,
    pa: &mut super::adapter::PlayerAdapter,
    reload: crate::route::AutoOriginalReload,
    pending_seek: i64,
    user_target: i64,
) {
    let carried_seek = commit_or_carry_seek(ps, pending_seek);
    start_original_trial_reload(ps, pa, reload, user_target);
    rearm_carried_seek(ps, pa, carried_seek);
}

/// An AUTOMATIC HLS-to-Original claim's tail. An accepted recovery is [`original_tail`]. A refused
/// one is not an ordinary rejection: the Auto worker exited to hand the action over, so the stream
/// has no producer, and a refused/lost Original decision leaves the HLS route fully intact —
/// rebuild that exact HLS Engine at the action's position rather than turning a recoverable probe
/// failure into the terminal playback screen. Settling the action `Rejected` is also what tells the
/// reopened worker not to ask again on this encoder (`route::finish_route_action`). Returns whether
/// the Engine was replaced.
fn land_automatic_original(
    ps: &mut crate::route::PlaybackSession,
    pa: &mut super::adapter::PlayerAdapter,
    action: &crate::route::ClaimedRouteAction,
    tail: crate::route::ClaimTail,
    pending_seek: i64,
    position_ns: i64,
) -> bool {
    let crate::route::ClaimTail::Rejected(msg) = tail else {
        return run_claim_tail(ps, pa, action, tail, pending_seek, position_ns);
    };
    if !crate::route::finish_route_action(ps, action, crate::route::RouteApplyResult::Rejected) {
        // A teardown or a fresh playback request superseded the action: `ps` may belong to
        // another item, and there is no retained HLS of ours to reopen.
        return false;
    }
    super::log(msg);
    let carried_seek = commit_or_carry_seek(ps, pending_seek);
    settle_reload(
        super::engine::reload_transcode(ps, pa, position_ns),
        "retained HLS reopen after rejected Original", ps,
    );
    rearm_carried_seek(ps, pa, carried_seek);
    true
}

/// An AUTOMATIC Original-to-HLS claim's tail. An accepted fallback is an ordinary prepared
/// transcode: settle the action, reload onto the HLS route at the action's position. A refused one
/// is not an ordinary rejection: the Auto worker exited to hand the action over, so the Original
/// has no producer left to carry on and the failure is raised for the pump's own recovery
/// (`demux_io_failed`). Returns whether the Engine was replaced.
fn land_automatic_hls(
    ps: &mut crate::route::PlaybackSession,
    pa: &mut super::adapter::PlayerAdapter,
    action: &crate::route::ClaimedRouteAction,
    tail: crate::route::ClaimTail,
    pending_seek: i64,
    position_ns: i64,
) -> bool {
    match tail {
        crate::route::ClaimTail::Retranscode => {
            land_prepared_route(
                ps,
                pa,
                action,
                pending_seek,
                position_ns,
                ("automatic Original-to-HLS", ""),
                super::engine::reload_transcode,
            );
            true
        }
        crate::route::ClaimTail::Rejected(msg) => {
            if !crate::route::finish_route_action(ps, action, crate::route::RouteApplyResult::Rejected) {
                // A teardown or a fresh playback request superseded the action: the failure
                // belongs to a playback that is gone, so it must not be raised on the next one.
                return false;
            }
            super::log(msg);
            // The refused fallback leaves the Original with no producer: the failure goes up in the
            // feed's own mailbox and comes back through the next sample, where the frame's
            // `demux_io_failed` arm (and so the ladder) finds it.
            pa.feed_command(plx_playback_contract::feed::FeedCommand::NoteFailure(
                plx_playback_contract::feed::FailureFlag::DemuxIoFailed,
            ));
            false
        }
        other => run_claim_tail(ps, pa, action, other, pending_seek, position_ns),
    }
}

/// Refused: restore the previous projection and keep playing what plays.
fn rejected_tail(
    ps: &mut crate::route::PlaybackSession,
    action: &crate::route::ClaimedRouteAction,
    msg: &str,
) {
    if crate::route::finish_route_action(ps, action, crate::route::RouteApplyResult::Rejected) {
        super::log(msg);
    }
}

/// A seek the pump is about to dispatch (in place, a reload, or a transcode landing's reload) while
/// the VIEWER holds a Pause can only land if the feed gate is open for its keyframe
/// (`FEED_TX.feed_allowed`), so the pump opens the bounded paused-seek preroll itself and owns closing
/// it (`settle_paused_seek`). The ownership is transport-wide (`FEED_TX.own_paused_seek`), so it
/// survives the reload this call may be followed by. A preroll a caller already opened (the
/// foreground restore) stays the caller's; a Pause the claim hold took (`claim_hold::owns_pause`)
/// is not the viewer's and is given back by the hold. The readout's preroll opens in the same call
/// (`BeginPausedSeek` is gated on its own Pause and is idempotent, as the feed's open is).
fn own_seek_under_pause(ps: &mut crate::route::PlaybackSession, pa: &mut super::adapter::PlayerAdapter,
    target_ns: i64) {
    if ps.readout().paused() && !super::claim_hold::owns_pause() {
        ps.step_readout(super::readout::ReadoutEvent::BeginPausedSeek);
        pa.feed_command(plx_playback_contract::feed::FeedCommand::OwnPausedSeek { target_ns });
    }
}

/// Test-only: how many times `settle_paused_seek` took the "viewer paused mid-seek" branch.
#[cfg(test)]
static MID_SEEK_CLAIMS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

fn settle_paused_seek(ps: &mut crate::route::PlaybackSession, pa: &mut super::adapter::PlayerAdapter, now: u32) {
    let step = pa.paused_seek_step(
        now, crate::route::flight_outstanding(), super::claim_hold::owns_pause(),
    );
    apply_paused_seek_step(ps, pa, step);
}

/// What the player does with the feed's verdict for one tick. The feed opens the preroll itself, so
/// the readout steps that open BEFORE it acts on `finish` (one call can report both: a sampled level
/// would read the pre-open value, and the film would play on under a Paused transport).
fn apply_paused_seek_step(ps: &mut crate::route::PlaybackSession, pa: &mut super::adapter::PlayerAdapter,
    step: plx_playback_contract::feed::PausedSeekStep) {
    #[cfg(test)]
    if step.mid_seek_claimed { MID_SEEK_CLAIMS.fetch_add(1, std::sync::atomic::Ordering::Relaxed); }
    if step.preroll_opened { ps.step_readout(super::readout::ReadoutEvent::BeginPausedSeek); }
    if step.finish { super::finish_paused_seek(ps, pa); }
}

/// No claim hold is up and no route flight is out: the two inputs of "the pump is doing no work for
/// a picture" that are not the readout's. The readout adds its own Pause (no preroll open: the feed
/// gate is shut) and its own pending seek, and asks after taking this frame's sample in
/// (`readout::Presentation::nothing_in_flight`), so a seek the feed itself just re-posted in this
/// tick counts. "Buffering…" over a player in that state is a spinner for nothing; the ladder
/// reports `PlaybackState::Paused` (the normal paused transport) instead. A Load still opening
/// (`Stage::Loading` -> Connecting) and an HLS rebuffer are real work and are decided before this
/// is asked.
fn nothing_in_flight() -> bool {
    !super::claim_hold::active() && !crate::route::flight_outstanding()
}

/// The playback trace generation `refuse601` already refused (0: none yet). One request is refused
/// once: its converted reload carries the same generation, so it plays.
static REFUSED_FOR: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

pub fn pump(ps: &mut crate::route::PlaybackSession, pa: &mut super::adapter::PlayerAdapter, now: u32) {
    if super::route_start::retire_if_server_stopped(ps, pa) {
        return;
    }
    // The HLS worker's report first, so this frame's samples carry it (the sparse facts take two of
    // its members) and the diagnostics read what the worker last said.
    pa.drain_abr(ps);
    // Capture before a terminal release can clear the callback mailbox, then capture every
    // surviving/replacement Engine after the frame, including its early-return paths. A callback
    // or timeout raised during this tick must reach reports before the app observes Error.
    pa.publish_feed_attempt_view(ps);
    // The HLS worker's carried link estimate is route's input, delivered here rather than read by it.
    ps.carry_readout_seed();
    deliver_container_names(ps);
    pump_frame(ps, pa, now);
    pa.publish_feed_attempt_view(ps);
}

/// Hand the route session the container's track names when the demuxer has published newer ones. The
/// route reads the copy it owns; this is the one place the names registry is read for it. One atomic
/// load per tick when nothing changed, so the common frame takes no lock.
pub(super) fn deliver_container_names(ps: &mut crate::route::PlaybackSession) {
    let stamp = SHARED.track_names_stamp.load(Acquire);
    if ps.container_names_stamp() != stamp {
        let names = SHARED.track_names_lenient().clone();
        ps.ingest_container_names(stamp, names);
    }
}

/// Hand `send` the side reader the route wants, once: the readout holds the newest wish (`None` is a
/// wish to stop) until this takes it, so a frame with no new wish sends nothing.
fn hand_over_side_reader_want(ps: &mut crate::route::PlaybackSession,
    send: impl FnOnce(plx_playback_contract::feed::FeedCommand)) {
    if let Some(want) = ps.readout().side_reader_want().cloned() {
        ps.step_readout(super::readout::ReadoutEvent::SideReaderWantTaken);
        send(plx_playback_contract::feed::FeedCommand::SyncSideReader(want));
    }
}

fn pump_frame(ps: &mut crate::route::PlaybackSession, pa: &mut super::adapter::PlayerAdapter, now: u32) {
    use plx_playback_contract::readout::PlaybackState;
    // A preroll closed by a path with no adapter in hand (`abandon_seek`) leaves its mute behind;
    // the one place every closer funnels through is the next tick.
    if !ps.readout().seek_preroll() {
        super::restore_preroll_volume(pa);
    }
    settle_paused_seek(ps, pa, now);
    // An Engine that failed for good last frame: let go of its pipeline and the video plane.
    if super::route_start::release_failed_engine(ps, pa) {
        return;
    }
    if !pa.is_live() {
        // A worker registered a replacement for a flight that has since ended: stop it now,
        // not whenever the next playback's Engine drains it.
        crate::route::discard_stale_flight_landing();
        set_state(ps, PlaybackState::Idle);
        return;
    }
    // Preserve the old per-frame ordering: diagnostic mirror, side-reader failure, then Load
    // settlement. Borrowing the Engine only for each step lets the adapter land its owned inbox.
    pa.publish_feed_diag(now, &LAST_FRAMES);
    if ps.readout().side_reader_failed() {
        ps.step_readout(super::readout::ReadoutEvent::Taken { edge: super::readout::Edge::SideReaderFailed });
        crate::route::side_subtitles_failed(ps);
    }
    // The route's wish for the side reader (a pick on a live remux) is handed to the feed here, the
    // frame after the route stated it.
    hand_over_side_reader_want(ps, |command| pa.feed_command(command));
    // The native Load may return before its Engine is installed. Only this main-thread landing
    // maps its worker token to the current attempt and settles the route, after installation.
    pa.drain_load_outcomes(ps);
    if let Some(attempt) = pa.active_feed_attempt() {
        let snapshot = super::timeline::TimelineSnapshot::from_feed(
            ps.readout(),
            super::duration_ns(ps).unwrap_or(0),
        );
        pa.publish_timeline(attempt, snapshot);
    }
    // The physical refusal probe precedes a route flight landing that may replace this feed.
    let refused_before = REFUSED_FOR.load(Acquire);
    pa.inject_refuse601(
        crate::route::playback_trace_generation(), crate::route::is_preview(ps), &REFUSED_FOR,
    );
    // A refusal the probe just raised is read by the failure branches below, in THIS frame, from the
    // readout: sample again so they see it, as for the stall watchdog's (a frame that raised
    // nothing pays no second sample).
    if REFUSED_FOR.load(Acquire) != refused_before {
        pa.publish_feed_attempt_view(ps);
    }
    // A failed open's recovery flight (the Original rollback's rebase, the unopened source's HLS
    // fallback, the refused native audio's conversion): land it, or wait for it. Ahead of every
    // failure branch below, whose flags stay up
    // for the whole flight and which would otherwise fail the Engine the landing replaces.
    if let Some(landing) = crate::route::take_ready_recovery_flight(ps) {
        land_recovery_flight(ps, pa, landing);
        return;
    }
    if crate::route::recovery_flight_outstanding() {
        hold_connecting_state(ps, ps.readout().raw().load.loading());
        return;
    }
    // `sf_load == 0` may leave no callable object, so this must precede the sf_ready wait below;
    // otherwise the pump returns Connecting forever and never consumes the explicit failure.
    if ps.readout().raw().flags.load_failed {
        if crate::route::preview_request(ps) {
            finish_preview_open_failure(ps, pa);
            return;
        }
        // The Load CALL's own refusal, on a Load the foreground lifecycle follows, is that
        // lifecycle's (#1018's retry). Only a refusal observed after the call returned is the ladder's.
        if settled_by_foreground(
            pa.foreground_follows_feed_load(),
            crate::route::route_start_status(pa.active_feed_attempt().expect("live feed attempt")),
        ) {
            return;
        }
        // The pipeline refused the open: the failed-open ladder decides. A refusal of natively fed
        // audio is recorded first (a queued write, selecting nothing). A stream the attempt can do without
        // (`recovery::suspect`: audio outside aac/ac3/eac3, whatever the refusal's number) is rebuilt
        // with the server converting it, once per request, when this Engine has shown no picture (a
        // reload after the request's first picture is such an Engine too). A stream `start` already
        // lacks, a rebuild that cannot fly, or a request with no rung left ends on the read-out the
        // viewer would have had; a refusal the machine does not handle (it is in `Failed`, or a
        // flight is outstanding) fails the Engine again, never silence.
        let loading = ps.readout().raw().load.loading();
        let refusal = load_refusal(ps);
        let native_audio = native_audio_fact(ps);
        let run = recover_failed_open(ps, loading, refusal, native_audio, "Audio Codec Not Supported");
        if let OpenRecoveryRun::DecodeInApp { offset_ns } = run {
            decode_in_app(ps, pa, offset_ns);
        }
        return;
    }
    // ---------- the Dolby Vision landing guard (#1166): a declared landing the firmware's splitter
    // would skip (the keyframe was held back at the landing) or that no position update follows turns
    // the declaration off for this play and reloads once. It runs BEFORE the native surround-audio
    // watchdog below and before the seek stuck-watchdog further down, which yield to a held landing
    // (they could neither retry the reopen nor reload with the node still sent). The order against
    // `native_audio_stall_fired` matters on a tie: a pipeline that took some video and audio in one
    // frame and then stopped accepting starts BOTH clocks on the next frame, and the audio verdict
    // writes an install-wide learned refusal for a fault that may be the Dolby Vision splitter's.
    if let Some(w) = pa.dv_landing_due(now, crate::route::flight_outstanding()) {
        if let Some(target) = withdraw_dolby_vision(ps, pa, &w) {
            settle_reload(
                super::engine::reload_at(ps, pa, target),
                "Dolby Vision withdrawn at a landing",
                ps,
            ); // REPLACES the engine (the feed's), return
            return;
        }
    }
    if pa.native_audio_stall_fired(now) {
        // The watchdog raised its refusal inside that call, after this frame's first sample: take
        // the sample the ladder is about to read, or a one-shot fire would be lost.
        pa.publish_feed_attempt_view(ps);
        // A trailer preview is direct-play only: its stall ends the preview, never the player's
        // read-out (as in the failure branches below).
        if crate::route::preview_request(ps) {
            finish_preview_open_failure(ps, pa);
            return;
        }
        // The stall is a refusal like `type=18`, and so is its ending: a combination the attempt
        // already lacks (a stored refusal is in `start`) fails the Engine like any refusal the ladder
        // has no rung for, never a spinner for good. Nothing fed natively (the watchdog needs more
        // than two channels of it, so only a race gets here) names no audio to confirm, and the
        // frame goes on.
        if let Some(audio) = native_audio_fact(ps) {
            let why = "it swallowed the feed: no picture";
            let loading = ps.readout().raw().load.loading();
            let run = recover_failed_open(ps, loading, super::recovery::LoadRefusal::Stalled, Some(audio), why);
            if let OpenRecoveryRun::DecodeInApp { offset_ns } = run {
                decode_in_app(ps, pa, offset_ns);
            }
            return;
        }
    }
    // A constructor still in flight reports the Load budget gate's timeout (the feed runs the
    // gate); no route decision below may treat it as a callable object.
    if let plx_playback_contract::feed::CtorGate::Awaiting { timeout } = pa.feed_ctor_gate() {
        if let Some(timeout) = timeout { note_load_timeout(timeout); }
        set_state(ps, PlaybackState::Connecting);
        return;
    }
    // ---------- the Original recovery, once it has to answer for itself ----------
    //
    // `recover_auto_to_original` deferred two irreversible things — the server-side stop and the
    // old route — because the probes that authorised the switch are a claim about a byte range
    // fetched seconds ago, not about the fresh open the reload above just performed. Here is where
    // that open is graded.
    //
    // **Decoded frames, not `loadCompleted`.** A completed Load is the pipeline accepting a
    // PAYLOAD DECLARATION; it says nothing about the source delivering. `frames` is zeroed by
    // `teardown(for_reload=true)` on the way into this reload (`SHARED::reset_session`), so a
    // non-zero count here belongs to the new source and to nothing else.
    if crate::route::original_recovery_pending() && ps.readout().raw().frames > 0 {
        crate::route::confirm_original_recovery(ps);
    }
    if ps.readout().raw().frames > 0 {
        // **The first decoded frame** (`route::on_decoded_frame`, one call per frame, each step once
        // per landing), in the one order its doc fixes: the container-name re-match of a remembered
        // subtitle (FO: the demuxer's track names exist now), the resume confirmation, and
        // **the write of a remembered choice back to the server** (W6): the landing parked it,
        // and a decoded frame is the evidence the television took the direct play with those
        // tracks. A refused direct play never gets here with it parked (its rescue installs a fresh
        // session), and the send only queues, so this frame never waits.
        crate::route::on_decoded_frame(ps);
        // The open produced a picture: the failed-open ladder is over (idempotent, so each frame may
        // say it). It is also where a facet-clean rescue is credited (`Proving`) and where a rescued
        // refusal is reported (`ReportRecovered`, once per request), through the driver; in
        // `Playing` the event is unhandled and asks for nothing.
        let _ = drive_open_recovery(ps, super::recovery::RecoveryEvent::FirstPicture, "");
    }
    pa.note_missing_era_callback();

    // The producer died before publishing a duration: the EOS path is gated on `duration_ns > 0`
    // so it can NEVER fire, and the player used to sit on a black screen forever with no error
    // and no exit. Surface it instead — BACK is already the escape.
    //
    // **Unless there is a way back**, which is the whole of the deferral above. Device,
    // 2026-08-29: the viewer asked for Original by hand on a film that was playing, the source URL
    // failed the way the same server's Original probe had failed forty seconds earlier, and this
    // line raised the failure read-out on a stream that had been working. A failed recovery is
    // worth a second reload; it is not worth the playback.
    // A worker may publish a more specific terminal code immediately before this generic Release
    // flag. Acquire is the matching hand-off; `error_now` can then report the transaction cause
    // instead of racing it into the generic producer bucket.
    if ps.readout().raw().flags.demux_io_failed {
        if crate::route::preview_request(ps) {
            finish_preview_open_failure(ps, pa);
            return;
        }
        let loading = ps.readout().raw().load.loading();
        let run = recover_failed_open(ps, loading, super::recovery::LoadRefusal::SourceFailed, None, "");
        if let OpenRecoveryRun::DecodeInApp { offset_ns } = run {
            decode_in_app(ps, pa, offset_ns);
        }
        return;
    }
    if ps.readout().raw().flags.demux_failed && ps.readout().raw().frames == 0 {
        if crate::route::preview_request(ps) {
            finish_preview_open_failure(ps, pa);
            return;
        }
        let raw = ps.readout().raw();
        let loading = raw.load.loading();
        // The conditional codec probe: the demuxer refused a stream whose BYTES disagree with the
        // Load's declaration — a stale PMS track list (a file replaced under the server, never
        // re-analyzed) or a transcode whose output the route described from the source's record —
        // and recorded what the file really carries. Reload ONCE with the corrected declaration:
        // a healthy open never sets the probe, so a healthy start pays nothing for this. The
        // probe rides the same sample as the `demux_failed` flag that gates this branch — the
        // demuxer publishes it before that flag — so a flag without its probe is impossible.
        let probe_v = raw.probe_actual_video;
        let probe_a = raw.probe_actual_audio;
        let retry_position = raw.seek_target_ns.unwrap_or(raw.playpos_ns);
        if (probe_v != 0 || probe_a != 0) && crate::route::take_codec_probe_retry(ps) {
            if probe_v != 0 {
                let v = match probe_v {
                    1 => "h264",
                    2 => "hevc",
                    3 => "av1",
                    _ => "vp9",
                };
                crate::route::set_stream_vcodec(ps, v);
            }
            if probe_a != 0 {
                let a = match probe_a {
                    1 => "ac3",
                    2 => "eac3",
                    3 => "aac",
                    _ => "dts",
                };
                crate::route::set_stream_acodec(ps, a);
            }
            super::log(&format!(
                "probe retry: the demuxer refused a stale declaration (video probe={probe_v}, audio probe={probe_a}); one reload with the codec the file really carries"
            ));
            // `reload_at` tears the failed Engine down — Starting -> Prepared keeps the same
            // transaction, so the retry mints a fresh attempt — reopens the same URL and
            // re-issues the Load with the corrected payload. Its teardown also clears
            // `demux_failed`, so this hook cannot re-fire for the attempt it is correcting;
            // if the corrected Load is refused too, the one-shot guard hands the next
            // mismatch to the ordinary failure read-out instead of a loop.
            let _ = settle_reload(super::engine::reload_at(ps, pa, retry_position), "probe retry reload", ps);
        } else {
            let run = recover_failed_open(ps, loading, super::recovery::LoadRefusal::SourceFailed, None, "");
            if let OpenRecoveryRun::DecodeInApp { offset_ns } = run {
                decode_in_app(ps, pa, offset_ns);
            }
        }
        return;
    }
    // ---------- one synchronized route/action state machine ----------
    //
    // User quality/track changes survive pre-roll and outrank automatic ABR. A simultaneous seek
    // is not a competing mailbox: it supplies this action's target, so the two become ONE reload
    // instead of either resetting the seek or rebuilding at the position the viewer already left.
    if ps.readout().raw().load.route_actions_ready {
        // A retranscode claim's PMS half may now be running on a worker (the freeze this fixes:
        // `execute_retranscode_claim` used to run `put_selection` + `/decision` right on this frame
        // thread). Drain any landing BEFORE trying to claim a new action — `ControlPhase` stays
        // `Applying` the whole time, so `claim_route_action` below never fires for the SAME action
        // while this is pending; it only starts returning `Some` again once `run_claim_tail`
        // settles the drained landing.
        let ready = crate::route::take_ready_flight(ps, ps.readout().pending_seek().is_some());
        if let Some(crate::route::ReadyFlight::Claim { action, tail, pending_seek, user_target }) = ready {
            // The claim held presentation at its offset for the flight (`claim_hold`). Give play
            // back only once the tail has run: a rejection left the same Engine playing, and an
            // accepted claim's reload has kept the pause, so the Play lands on the NEW stream.
            let hold = super::claim_hold::take(action.serial());
            // The hold paused a PLAYING stream iff `hold`. When it did, record the viewer's real
            // transport goal (play) now, before the tail starts the fresh stream, so the
            // landing's Feed start reflects what the viewer wants rather than the pause the hold
            // itself imposed for the flight. Without this the landing reads the hold's own pause
            // as the viewer's goal and starts the fresh stream paused: no Play op, no
            // `startup_play_commanded`, and the Initial prime's gate never opens — the 4K->480p
            // retranscode stall. A hold-less flight (a direct Original) left the transport
            // untouched, so it records no goal and the landing falls back to the stream's own
            // transport state — a goal written for the no-hold case would start that recovery
            // paused and interrupt an automatic upgrade.
            if hold {
                ps.set_reconfigure_pause_goal(false);
            }
            let replaced = if matches!(
                action.intent,
                crate::route::RouteIntent::Automatic(crate::route::AutomaticRouteIntent::HlsToOriginal { .. })
            ) {
                land_automatic_original(ps, pa, &action, tail, pending_seek, user_target)
            } else if matches!(
                action.intent,
                crate::route::RouteIntent::Automatic(crate::route::AutomaticRouteIntent::OriginalToHls { .. })
            ) {
                land_automatic_hls(ps, pa, &action, tail, pending_seek, user_target)
            } else {
                run_claim_tail(ps, pa, &action, tail, pending_seek, user_target)
            };
            // The reload (if any) has moved the Engine onto the new stream; only now is the
            // encoder it replaced safe to stop.
            crate::route::retire_superseded_encoder();
            super::claim_hold::release(ps, pa, hold);
            // A tail that started a fresh Feed already consumed and cleared the goal; one that
            // didn't (a rejection, or a landing onto the original) must not leak it into a later
            // start, where it would override that start's own transport state.
            ps.clear_reconfigure_pause_goal();
            if replaced {
                return;
            }
        } else if let Some(crate::route::ReadyFlight::Seek { serial, target_ns, verdict }) = ready {
            let reloads = matches!(verdict, crate::route::SeekVerdict::Install(_));
            seek_landing_tail(ps, pa, serial, target_ns, verdict);
            if reloads {
                return; // A fresh physical feed replaced this attempt.
            }
        } else if let Some(action) = crate::route::claim_route_action() {
            let pending_seek = ps.readout().pending_seek().unwrap_or(-1);
            let current_pos = ps.readout().raw().playpos_ns.max(0);
            let user_target = if pending_seek >= 0 {
                pending_seek
            } else {
                current_pos
            };
            match action.intent.clone() {
                crate::route::RouteIntent::User(intent) => match intent {
                    crate::route::UserRouteIntent::Retranscode => {
                        // "Retranscode" means "reconcile at claim" (issue #266): the route PMS
                        // half decides between the enhancement and today's rebuild.
                        let secs = user_target / 1_000_000_000;
                        match crate::route::execute_retranscode_claim(ps, &action, secs, pending_seek, user_target) {
                            crate::route::RetranscodeClaimDispatch::Sync(tail) => {
                                if run_claim_tail(ps, pa, &action, tail, pending_seek, user_target) {
                                    return;
                                }
                                                }
                            crate::route::RetranscodeClaimDispatch::Pending
                            | crate::route::RetranscodeClaimDispatch::PendingNoHold => {
                                // A worker now owns the PMS half. The drain at the top of this
                                // block picks up the landing later — never a join, never a
                                // blocked frame. Meanwhile presentation holds at the claim offset:
                                // the landing reloads THERE, so letting the flight play would show
                                // the same seconds twice.
                                super::claim_hold::engage(ps, pa, action.serial());
                                                }
                        }
                    }
                    crate::route::UserRouteIntent::NativeAudioReload => {
                        honour_displaced_pick(ps, pa, action.displaced_pick, "NativeAudioReload");
                        native_audio_tail(ps, pa, &action, pending_seek, user_target);
                        return;
                    }
                    crate::route::UserRouteIntent::AdaptiveReload => {
                        if crate::route::is_transcoding(ps) {
                            // The same flight as a seek, owned by this claim: PMS on a worker,
                            // the claim stays `Applying`, and the drain at the top of this block
                            // lands it (`adaptive_tail`). The claim offset is where the reload
                            // goes, so presentation holds there for the flight.
                            honour_displaced_pick(ps, pa, action.displaced_pick, "AdaptiveReload");
                            let secs = user_target / 1_000_000_000;
                            match crate::route::execute_adaptive_reload_claim(ps, &action, secs, pending_seek, user_target) {
                                crate::route::RetranscodeClaimDispatch::Sync(tail) => {
                                    if run_claim_tail(ps, pa, &action, tail, pending_seek, user_target) {
                                        return;
                                    }
                                                        }
                                crate::route::RetranscodeClaimDispatch::Pending
                                | crate::route::RetranscodeClaimDispatch::PendingNoHold => {
                                    super::claim_hold::engage(ps, pa, action.serial());
                                                        }
                            }
                        } else {
                            honour_displaced_pick(ps, pa, action.displaced_pick, "AdaptiveReload");
                            crate::route::finish_route_action(
                                ps,
                                &action,
                                crate::route::RouteApplyResult::Prepared,
                            );
                            if pending_seek >= 0 {
                                crate::route::commit_user_seek();
                            }
                            super::log(&format!(
                                "route transition: adaptive direct reload at {}s{}",
                                user_target / 1_000_000_000,
                                if pending_seek >= 0 { " + seek" } else { "" },
                            ));
                            settle_reload(
                                super::engine::reload_at(ps, pa, user_target),
                                "adaptive direct reload", ps,
                            );
                            return;
                        }
                    }
                    crate::route::UserRouteIntent::RecoverOriginal(cause) => {
                        // The viewer's Original pick: the same flight as an enhancement release.
                        // PMS (the Part admission, the replacement remux) runs on a worker while
                        // presentation holds at the claim offset; the drain lands it.
                        let secs = user_target / 1_000_000_000;
                        match crate::route::execute_recover_original_claim(
                            ps, &action, secs, cause, pending_seek, user_target,
                        ) {
                            crate::route::RetranscodeClaimDispatch::Sync(tail) => {
                                if run_claim_tail(ps, pa, &action, tail, pending_seek, user_target) {
                                    return;
                                }
                                                }
                            crate::route::RetranscodeClaimDispatch::Pending
                            | crate::route::RetranscodeClaimDispatch::PendingNoHold => {
                                super::claim_hold::engage(ps, pa, action.serial());
                                                }
                        }
                    }
                },
                crate::route::RouteIntent::Automatic(intent) => {
                    match intent {
                        crate::route::AutomaticRouteIntent::OriginalToHls {
                            ticket,
                            conservative_kbps,
                            position_ns,
                        } => {
                            let position_ns = position_ns.max(0);
                            let secs = position_ns / 1_000_000_000;
                            if ticket != action.ticket {
                                crate::route::finish_route_action(
                                    ps,
                                    &action,
                                    crate::route::RouteApplyResult::Cancelled,
                                );
                                super::log("auto: discarded fallback action whose ticket changed before claim");
                                return;
                            }
                            // The Auto worker exited to hand this action over; the Original is
                            // what plays until the landing replaces it. The replacement's PUT +
                            // `/decision` run on a worker (nothing is held or paused for them: a
                            // refusal or a success is the only thing that changes the stream), and
                            // the drain at the top of this block lands them.
                            match crate::route::execute_auto_hls_claim(
                                ps,
                                &action,
                                secs,
                                conservative_kbps,
                                pending_seek,
                                position_ns,
                            ) {
                                crate::route::RetranscodeClaimDispatch::Sync(tail) => {
                                    if land_automatic_hls(ps, pa, &action, tail, pending_seek, position_ns) {
                                        return;
                                    }
                                                        }
                                crate::route::RetranscodeClaimDispatch::Pending
                                | crate::route::RetranscodeClaimDispatch::PendingNoHold => {
                                                        }
                            }
                        }
                        crate::route::AutomaticRouteIntent::HlsToOriginal {
                            ticket,
                            evidence_kbps: _,
                            position_ns,
                        } => {
                            let position_ns = position_ns.max(0);
                            let secs = position_ns / 1_000_000_000;
                            if ticket != action.ticket {
                                crate::route::finish_route_action(
                                    ps,
                                    &action,
                                    crate::route::RouteApplyResult::Cancelled,
                                );
                                super::log("auto: discarded Original action whose ticket changed before claim");
                                return;
                            }
                            // The Auto worker exited to hand this action over, and it published
                            // `position_ns`: the same flight as the viewer's own Original pick. A
                            // direct Original never reaches PMS, so its flight is a frame or two
                            // and the stream keeps playing; a remux's PUT + `/decision` holds the
                            // picture at `position_ns`, because the landing reloads THERE.
                            match crate::route::execute_recover_original_claim(
                                ps,
                                &action,
                                secs,
                                crate::route::RecoveryCause::Automatic,
                                pending_seek,
                                position_ns,
                            ) {
                                crate::route::RetranscodeClaimDispatch::Sync(tail) => {
                                    if land_automatic_original(ps, pa, &action, tail, pending_seek, position_ns) {
                                        return;
                                    }
                                                        }
                                crate::route::RetranscodeClaimDispatch::Pending => {
                                    super::claim_hold::engage(ps, pa, action.serial());
                                                        }
                                crate::route::RetranscodeClaimDispatch::PendingNoHold => {
                                                        }
                            }
                        }
                    }
                }
            }
        }
    }

    // ---------- an in-place seek is still resolving (flushed, not yet re-anchored): COALESCE.
    // Firing another now would race the demux reopen (rapid tap-seeks) and the rebase guard would
    // then eat everything → stuck. Hold the latest requested target (it stays the readout's pending target) and
    // process it once this seek anchors. If it hasn't anchored within SEEK_STUCK_MS the reopen was
    // lost — first RETRY the reopen cheaply (re-interrupt the demuxer + re-arm), and only fall back
    // to a full (slow) reload if the retries also fail.
    // SEEK_STUCK_MS must comfortably exceed a real reopen+av_seek+first-keyframe on 4K HEVC over
    // PMS (~700ms): the retry closes the demux socket, so a premature watchdog KILLS an open that
    // was about to succeed and restarts it — at 500ms a rapid tap-burst self-DoS'd into the
    // reload fallback every time (caught by the seek_rapid harness cases).
    // A claim's worker owns `ControlPhase::Applying` and has no vocabulary for a seek reload
    // reaching in and replacing the encoder/engine out from under it (see `route::flight_outstanding`'s own
    // doc). Skip both this branch and the plain pending-seek branch below while one is in flight —
    // the pending target stays exactly where it is and the retranscode/native-audio tail re-checks it
    // once the claim lands (`commit_or_carry_seek`).
    const SEEK_STUCK_MS: u32 = 1200;
    // Route owns the pending target and commits it before the native demux can read a retry.
    if pa.stuck_seek_due(now, ps.readout().feed_allowed(), crate::route::flight_outstanding(), SEEK_STUCK_MS) {
        let taken = super::take_pending_seek(ps, pa);
        let pending = taken.map_or(-1, |t| t.target_ns);
        // The feed's own in-place target rides the sample (`FeedRaw::seek_target_ns`): the seek it
        // belongs to was applied in an earlier frame, so the pump-start sample holds it.
        let tgt = if pending >= 0 { pending } else { ps.readout().raw().seek_target_ns.unwrap_or(-1) }.max(0);
        if pending >= 0 { crate::route::commit_user_seek(); }
        match pa.retry_stuck_seek(now, tgt, !seek_reloads(seek_mode(ps))) {
            plx_playback_contract::feed::StuckSeekAction::Retried(retries) => {
                super::log(&format!(
                    "seek: in-place stuck → retry reopen at {}s (#{}) coalesced={}",
                    tgt / 1_000_000_000, retries, taken.map_or(0, |t| t.coalesced),
                ));
            }
            plx_playback_contract::feed::StuckSeekAction::Reload => {
                super::log(&format!("seek: in-place stuck → reload at {}s", tgt / 1_000_000_000));
                settle_reload(super::engine::reload_at(ps, pa, tgt), "stuck in-place seek reload", ps);
                return;
            }
        }
    }
    // ---------- pending seek: flush, drop queued AUs, re-point the demux ----------
    let t = ps.readout().pending_seek().unwrap_or(-1);
    // A live transcode has NO Content-Length (file_size stays -1), so gate it on duration
    // only and seek by RESTARTING the transcode at a time &offset (below). Direct-play
    // seeks in place via av_seek.
    let is_transcode = crate::route::is_transcoding(ps);
    if pa.pending_seek_ready(seek_reloads(seek_mode(ps)))
        && t >= 0
        && !crate::route::original_recovery_pending()
        && !crate::route::flight_outstanding() // see the SEEK_STUCK_MS branch's doc above
        // coalesce: don't stack in-place seeks. Not in `Reload` mode: a refused native `Seek`
        // leaves the engine flushed and re-posts the target for exactly this branch to reload at.
        && super::duration_ns(ps).unwrap_or(0) > 0
        && (is_transcode || ps.readout().raw().file_size > 0)
    {
        // Taken here so BOTH branches consume this seek's requests; the feed's copy goes with it.
        let coalesced = super::take_pending_seek(ps, pa).map_or(0, |taken| taken.coalesced);
        // Consumed by whichever branch lands this seek: a transcode seek is a fresh Load anyway.
        let rate_reload = ps.readout().rate_reload();
        ps.step_readout(super::readout::ReadoutEvent::RateReloadTaken);
        let t = t.max(0);
        // TRANSCODE seek: restart the encode at the new &offset and do a FULL RELOAD (fresh Load).
        // A flush()+refeed of the new start.mkv left a STALE GStreamer segment → visual artifacts on
        // the jump (the same class of bug the in-place seek cured for direct-play; a transcode can't
        // use the in-place path — the stream content itself changes at the new offset). A fresh Load
        // rebuilds the segment by construction. reload_transcode REPLACES the ENGINE, so `eng`
        // dangles after it — return immediately and let the next pump() tick drive the fresh engine.
        if is_transcode {
            // The rebuild is a FLIGHT: planned here, PMS on a worker, installed by the drain at
            // the top of the route block on a later frame. Presentation holds under the Seeking
            // spinner meanwhile; a refused rebuild abandons the seek there.
            start_transcode_seek_flight(ps, pa, t, false);
            return;
        }
        // DIRECT-PLAY seek: IN-PLACE — pause, av_seek the demuxer and, in feed_stream on the first
        // post-seek keyframe, re-anchor the GStreamer segment WITHOUT a reload/decoder re-init (no
        // A/V-resync glitch): `Segment` = flush here + setTimeToDecode + sendSegmentEvent (Kodi),
        // `NativeSeek` = LG's public Seek(millis), which flushes by itself. A plain flush()+refeed
        // with NO fresh segment left a STALE GStreamer segment → the HW sink stops draining ~48 s
        // later → permanent BufferFull + "Playing error" (root-caused by decompiling
        // libpipeline/lxvideosink; see engine::reload_at). `Reload` mode — selected for a set
        // with no video plane or a layout-unknown library `Seek` is not tested on (see
        // plx_play_feed::seek::select_seek_mode), or set by feed_stream when sendSegmentEvent can't find the
        // pipeline or a native Seek is refused — is reload-per-seek. reload_at REPLACES the ENGINE,
        // so `eng` dangles after it: return immediately.
        if seek_must_reload(seek_mode(ps), super::play_rate_permille(ps), rate_reload) {
            crate::route::commit_user_seek();
            own_seek_under_pause(ps, pa, t);
            settle_reload(super::engine::reload_at(ps, pa, t), "direct seek reload", ps);
            return;
        }
        // Pause, user-held seek reuse and eventual prime all share one actuator state. In
        // particular a seek from Paused must not issue a redundant native Pause, and a result from
        // an older transition must not authorize flushing this session's queues.
        match pa.prepare_seek_pause() {
            plx_playback_contract::feed::SeekPause::Ready => {}
            plx_playback_contract::feed::SeekPause::Refused => {
                super::log("seek(in-place): Starfish refused Pause → reload fallback");
                crate::route::commit_user_seek();
                own_seek_under_pause(ps, pa, t);
                settle_reload(super::engine::reload_at(ps, pa, t), "seek reload after native Pause refusal", ps);
                return;
            }
            plx_playback_contract::feed::SeekPause::Stale => {
                super::log("seek(in-place): stale Pause result → reload fallback");
                crate::route::commit_user_seek();
                own_seek_under_pause(ps, pa, t);
                settle_reload(super::engine::reload_at(ps, pa, t), "seek reload after stale Pause", ps);
                return;
            }
            plx_playback_contract::feed::SeekPause::Busy => {
                super::requeue_seek(ps, pa, t);
                super::log("seek(in-place): clock transition busy; retrying");
                return;
            }
        }
        if !pa.begin_seek_discontinuity() {
            crate::route::commit_user_seek();
            super::log("seek(in-place): native callback epoch retired → reload fallback");
            own_seek_under_pause(ps, pa, t);
                        settle_reload(
                super::engine::reload_at(ps, pa, t),
                "seek reload after retired native epoch", ps,
            );
            return;
        }
        // This is the applied media boundary: everything after it belongs to the new playhead.
        // Advancing earlier would strand the live worker when PMS/native refused; advancing later
        // would let pre-seek ABR evidence publish while queues are already being reset.
        crate::route::commit_user_seek();
        pa.apply_inplace_seek_pre(t, seek_mode(ps), coalesced);
        own_seek_under_pause(ps, pa, t);
        pa.apply_inplace_seek_post(t, now);
    }

    let load = pa.tick_feed_load(now, crate::route::is_segmented_hls(ps), SPEED_AUDIO_DEADLINE_MS);
    if let Some(timeout) = load.timeout { note_load_timeout(timeout); }
    if let Some(reason) = load.speed_fallback { super::fall_back_from_speed(ps, pa, reason); }

    // Load and speed policy are settled. Borrow the physical feed for ACB, feeding and EOS.
    let physical = pa.tick_feed_presentation(
        crate::route::is_segmented_hls(ps), crate::route::stream_immersive(ps),
    );

    // ---------- the derived UI state and the end of stream ----------
    // The end of stream: the producer hit file EOF (`eos_pushed`: all AUs to the end were fed) and
    // the pipeline has now played out to within EOS_TAIL of the duration, or reported its own
    // end-of-stream (`Shared::pipeline_eos`). The readout latches it so app.rs tears the player
    // down at the credits instead of freezing on the last frame. Paused at the end stays paused
    // (playpos won't climb) — correct; it fires when resumed. Any seek clears it
    // (`request_seek`), so seeking back from the end doesn't re-trigger.
    // The state (see `PlaybackState`'s doc): a seek in flight outranks "we have frames", because
    // the frames on the panel are the PRE-seek ones and the HUD must show the target, not them.
    // Both derive in the readout (`readout::PlayerReadout::step`, `Presented`) from a sample taken
    // right after the tick, for the attempt this frame belongs to. A frame that returned early
    // derived nothing, as before.
    if let Some(attempt) = pa.active_feed_attempt() {
        if let Some(sample) = pa.sample_feed() {
            let was_ended = ps.readout().ended();
            if ps.present(attempt, physical, sample, nothing_in_flight()) && !was_ended && ps.readout().ended() {
                let (pos, dur) = (ps.readout().raw().playpos_ns, sample.attempt.duration_ns.unwrap_or(0));
                super::log(&format!(
                    "EOS reached: playpos={}s/{}s → ended",
                    pos / 1_000_000_000,
                    dur / 1_000_000_000
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use plx_play_feed::shared::{FEED_SHARED, FEED_TX};

    /// **The side reader the route wants reaches the feed once.** The route states a wish
    /// (`ReadoutEvent::SideReaderWanted`), the pump's next frame hands it over as one
    /// `FeedCommand::SyncSideReader` and the readout forgets it, so a frame with no new wish sends
    /// nothing; two wishes before a frame are one command carrying the newest; a stop is a command
    /// too (`SyncSideReader(None)`).
    #[test]
    fn a_wanted_side_reader_is_handed_to_the_feed_once_and_a_stop_is_a_command_too() {
        use plx_playback_contract::feed::FeedCommand;
        use plx_playback_contract::subtitle::Spec;
        use crate::player::readout::ReadoutEvent::SideReaderWanted;
        let spec = |ordinal| std::sync::Arc::new(Spec {
            url: String::new(), offset_ns: 0, part_kbps: 0, ordinal, from_playhead: false, delta: None,
        });
        let frame = |ps: &mut crate::route::PlaybackSession| {
            let mut sent = Vec::new();
            super::hand_over_side_reader_want(ps, |command| sent.push(command));
            sent
        };
        let mut ps = crate::route::PlaybackSession::IDLE;
        assert!(frame(&mut ps).is_empty(), "no wish, no command");

        ps.step_readout(SideReaderWanted(Some(spec(2))));
        ps.step_readout(SideReaderWanted(Some(spec(5))));
        let sent = frame(&mut ps);
        assert!(matches!(sent.as_slice(), [FeedCommand::SyncSideReader(Some(s))] if s.ordinal == 5),
            "the newest wish is the one command: {sent:?}");
        assert_eq!(ps.readout().side_reader_want(), None, "handing it over took it");
        assert!(frame(&mut ps).is_empty(), "an unchanged next frame sends nothing");

        ps.step_readout(SideReaderWanted(None));
        let sent = frame(&mut ps);
        assert!(matches!(sent.as_slice(), [FeedCommand::SyncSideReader(None)]), "the stop: {sent:?}");
        assert!(frame(&mut ps).is_empty(), "the stop is sent once");
    }

    /// **Who settles a failed Load: the Load call's own synchronous result is the foreground
    /// lifecycle's, anything the pump sees after the call returned is the ladder's.** The pump
    /// leaves a refusal alone only when BOTH hold: the lifecycle follows that Engine's Load, and
    /// the reducer says the call itself failed. (`type=18` after `Started`, a stall, a timeout and
    /// a Load nobody foreground follows all stay the ladder's.)
    #[test]
    fn only_a_followed_loads_own_failure_is_left_to_the_foreground() {
        use super::settled_by_foreground;
        use crate::route::RouteStartStatus as S;
        for start in [S::Pending, S::Started, S::Stale] {
            assert!(!settled_by_foreground(true, start), "{start:?}: not the call's own failure");
        }
        assert!(settled_by_foreground(true, S::Failed));
        assert!(!settled_by_foreground(false, S::Failed), "a Load no foreground machine follows is the ladder's");
    }

    /// **A refusal's code is read with its flag from ONE sample.** A worker publishes the code
    /// before the flag; the feed loads the flag first, and the drain reads both from that sample, so
    /// a code written (or cleared) after the sample cannot turn the refusal into `Refused { num: 0 }`.
    #[test]
    fn a_refusals_code_is_read_with_its_flag_from_one_sample() {
        use std::sync::atomic::Ordering::{Relaxed, Release};
        let _guard = plx_base::testlock::serial();
        crate::player::reset_session_for_test();
        let attempt = crate::route::RouteStartAttempt::fixture();
        let mut ps = crate::route::PlaybackSession::IDLE;
        ps.begin_attempt_view(attempt, super::super::attempt_view::Backend::Feed);
        FEED_SHARED.dg_cb_err_num.store(601, Relaxed);
        FEED_SHARED.load_failed.store(true, Release);
        assert!(ps.ingest_feed_facts(attempt, plx_play_feed::feed_pump::native_feed_facts()));
        // The mailbox moves on after the sample (a reload cleared it, a later callback replaced it).
        FEED_SHARED.dg_cb_err_num.store(0, Relaxed);
        assert!(ps.readout().raw().flags.load_failed);
        assert_eq!(super::load_refusal(&ps), super::super::recovery::LoadRefusal::Refused { num: 601 });
        FEED_SHARED.load_timed_out.store(true, Release);
        assert!(ps.ingest_feed_facts(attempt, plx_play_feed::feed_pump::native_feed_facts()));
        assert_eq!(super::load_refusal(&ps), super::super::recovery::LoadRefusal::TimedOut);
        // The ladder's "no frames" is the sample's too, not the mailbox's.
        FEED_SHARED.frames.store(5, Relaxed);
        assert!(ps.ingest_feed_facts(attempt, plx_play_feed::feed_pump::native_feed_facts()));
        FEED_SHARED.frames.store(0, Relaxed);
        assert!(!super::open_facts(&ps, None).no_frames);
        crate::player::reset_session_for_test();
    }

    /// **The learner is reached through its injected recorder and nowhere else.** A named write is
    /// the pipeline's own refusal: it records the facet the machine put on the effect once and
    /// consumes the flag that raised it. A proof is a facet-clean rescue: it records its facet and
    /// touches no flag. Neither changes the process-wide capability answer (`devcaps::caps()` is
    /// write-once; only a play start, `devcaps::start_caps`, reads the record): the closing section
    /// runs the REAL recorder under a temporary session and reads the answer before the queued write
    /// lands, and the record after it.
    #[test]
    fn confirm_refusal_writes_only_through_the_injected_recorder() {
        use super::super::recovery::Evidence;
        use plx_platform::devcaps::Facet;
        let _guard = plx_base::testlock::serial();
        let _set = plx_platform::devcaps::firmware_on_this_thread_for_test(
            "fw",
            "B",
            plx_platform::devcaps::Caps { audio: "aac,ac3,eac3,opus".into(), ..plx_platform::devcaps::Caps::assumed() },
        );
        let _session = plx_plex::plex::session::TempSession::new("confirm-refusal-recorder");
        let before = plx_platform::devcaps::caps();
        let recorded = std::cell::RefCell::new(Vec::new());
        let recorder = |facet: &Facet| {
            recorded.borrow_mut().push(facet.clone());
            true
        };
        let opus6 = Facet::Audio { codec: "opus".into(), channels: 6 };
        let dts6 = Facet::Audio { codec: "dts".into(), channels: 6 };
        // The session the pump's ingestion would have fed: the refusal is the readout's to take.
        let attempt = crate::route::RouteStartAttempt::fixture();
        let mut ps = crate::route::PlaybackSession::IDLE;
        ps.begin_attempt_view(attempt, super::super::attempt_view::Backend::Feed);
        let ingest = |ps: &mut crate::route::PlaybackSession| {
            assert!(ps.ingest_feed_facts(attempt, plx_play_feed::feed_pump::native_feed_facts()));
        };

        // Named, with no flag raised: nothing to write.
        crate::player::reset_session_for_test();
        ingest(&mut ps);
        assert!(!super::confirm_refusal(&mut ps, &opus6, Evidence::Named, "test", recorder));
        assert!(recorded.borrow().is_empty());

        // Named, flag raised: the effect's facet is recorded once, and the flag is consumed.
        FEED_SHARED.raise_native_audio_refused();
        ingest(&mut ps);
        assert!(super::confirm_refusal(&mut ps, &opus6, Evidence::Named, "test", recorder));
        assert_eq!(*recorded.borrow(), [opus6.clone()]);
        assert!(!super::confirm_refusal(&mut ps, &opus6, Evidence::Named, "test", recorder), "the refusal flag was consumed");
        assert_eq!(recorded.borrow().len(), 1);

        // A proof: its own facet, and a raised flag is not its to consume.
        FEED_SHARED.raise_native_audio_refused();
        ingest(&mut ps);
        assert!(super::confirm_refusal(&mut ps, &dts6, Evidence::Proof, "test", recorder));
        assert_eq!(*recorded.borrow(), [opus6.clone(), dts6]);
        assert!(ps.readout().native_audio_refused(), "a proof touches no flag");
        crate::player::reset_session_for_test();
        ps.step_readout(super::super::readout::ReadoutEvent::SessionReset);

        // The injected recorder was the only writer: the install's record holds nothing yet.
        plx_base::storage_worker::drain_for_test();
        assert!(plx_plex::plex::session::peek().native_audio_refused().is_empty(), "the fake recorder wrote nothing");

        // The real recorder, same call: the write is queued and the answer has not moved by the time
        // `confirm_refusal` returns; the record holds the row once the queue has drained.
        FEED_SHARED.raise_native_audio_refused();
        ingest(&mut ps);
        assert!(super::confirm_refusal(&mut ps, &opus6, Evidence::Named, "test", plx_plex::plex::session::record_named_refusal));
        assert!(std::sync::Arc::ptr_eq(&before, &plx_platform::devcaps::caps()), "queueing the write moved no global");
        plx_base::storage_worker::drain_for_test();
        assert_eq!(plx_plex::plex::session::peek().native_audio_refused().len(), 1, "the real recorder wrote the row");
        // Installing the record moves nothing either; the play-start edge is what reads it, and a named row applies at once.
        assert!(before.audio_supports("opus", 6), "sanity: announced before the record is read");
        plx_plex::plex::session::save(&plx_plex::plex::session::peek());
        assert!(std::sync::Arc::ptr_eq(&before, &plx_platform::devcaps::caps()), "installing the record moves no global");
        let start = plx_platform::devcaps::start_caps(&plx_plex::plex::session::peek().applied_refusals());
        assert!(!start.audio_supports("opus", 6), "the start value lacks the named row's Opus 5.1");
        assert!(start.audio_supports("opus", 2), "a surround refusal keeps the codec to stereo");
        crate::player::reset_session_for_test();
    }
    use crate::player::readout::stream_ended;
    use super::{
        commit_or_carry_seek, rearm_carried_seek,
        hls_buffered_ms, native_clock_silence_ms, native_clock_stopped,
        prime_before_play, rebuffer_request_is_current, should_begin_hls_rebuffer,
        CLOCK_WATCH_SINCE_MS, NATIVE_CLOCK_STOPPED_MS,
    };

    /// **The pipeline's own end-of-stream ends the film even short of the duration.** C1,
    /// 2026-10-20: audio ran 3 s past the last video frame, the clock stopped at 5894 s of
    /// 5897 s after the EOS, and the player sat on the last frame for good because the playhead
    /// never came within a second of the duration.
    #[test]
    fn the_pipelines_end_of_stream_ends_a_film_whose_audio_outlasts_its_video() {
        const S: i64 = 1_000_000_000;
        assert!(!stream_ended(5894 * S, 5897 * S, false), "short of the duration, nothing said");
        assert!(stream_ended(5894 * S, 5897 * S, true), "the pipeline said it reached the end");
        assert!(stream_ended(5896_500_000_000, 5897 * S, false), "within the tail, as before");
        assert!(!stream_ended(5894 * S, 0, false), "no duration, no arithmetic");
    }
    use std::sync::atomic::Ordering::{Relaxed, Release};

    /// The device's measured position-callback period, so the replay below advances in the same
    /// steps the television reports in (`vtick=5 vgap=201ms`; `plx_play_feed::sf_event::sf_on_event`).
    const POSITION_PERIOD_MS: u32 = 201;

    /// Put the three atomics `hls_buffered_ms` samples back where a fresh session leaves them, so
    /// a replay cannot leak a frozen playhead into another test holding the same serial lock.
    fn clear_hls_reserve_sample() {
        FEED_SHARED.hls_video_tail_ns.store(-1, Release);
        FEED_SHARED.hls_audio_tail_ns.store(-1, Release);
        FEED_SHARED.disp_base.store(0, Relaxed);
        FEED_SHARED.playpos_ns.store(0, Relaxed);
    }

    /// **Regression: `pipe_abr_down_collapse` on the television, 2026-09-02.** A 40000 kbps link
    /// collapsed to 500 kbps at t=25 s while a 20000 kbps rung was active. The controller's
    /// descent was correct once it could run, but it could not run for 82 s, and the case failed
    /// its `pos_climb` gate with 38 s of progress in a 120 s run.
    ///
    /// What stopped it is the terminal boundary being defined arithmetically. Segment 9 was
    /// demuxed to a video tail of ~29.96 s; the pipeline then stopped reporting a position
    /// altogether (`play=0pm vtick=0 vgap=0ms`, log line 248, holding `pos=29s` for the next
    /// 77 s) with the playhead parked SHORT of that tail, because the last fed access units
    /// cannot be presented until the ones behind them arrive. So `hls_buffered_ms` stuck at a
    /// small positive remnant forever, `should_begin_hls_rebuffer`'s `buffered_ms == Some(0)`
    /// never became true, no `hls: auto-rebuffer pause` was ever written, and the worker's
    /// `StallGuard` — which reads that same hold as `terminal_hold_started` — never abandoned the
    /// fetch. The 5.17 MB object therefore ran to completion on a 500 kbps link
    /// (`open_probe_ms=82431 total_ms=82682`, log line 323) before any rung decision existed.
    ///
    /// The physical fact the arithmetic cannot see is that the clock has STOPPED. This replays
    /// the numbers the log carries and asserts the hold begins about a second after it stops,
    /// not when the fetch completes.
    #[test]
    fn a_stopped_native_clock_reaches_the_terminal_boundary_the_arithmetic_never_will() {
        let _serial = plx_base::testlock::serial();
        // Segment 9 demuxed: 48 video AUs at 24 fps and 94 AAC frames, ending the tenth 2 s
        // segment of the stream (`hls: segment=9 ... v=48 a=94`, log line 235).
        FEED_SHARED.hls_video_tail_ns.store(29_958_000_000, Release);
        FEED_SHARED.hls_audio_tail_ns.store(29_980_000_000, Release);
        FEED_SHARED.disp_base.store(0, Relaxed);
        // The playhead then froze. The log prints whole seconds, so the exact remnant is not
        // recoverable from it; what the log does prove is that it never reached zero, because the
        // auto-rebuffer line that an exact zero writes was never written in those 77 s.
        FEED_SHARED.playpos_ns.store(29_700_000_000, Relaxed);
        let buffered_ms = hls_buffered_ms();
        // Clear the shared sample BEFORE the first assertion: `testlock::serial()` recovers a
        // poisoned lock, so a panic past this point would otherwise leak a frozen playhead into
        // every later serial test (see the test-suite pollution note in `lib.rs`).
        clear_hls_reserve_sample();
        assert_eq!(
            buffered_ms,
            Some(258),
            "the arithmetic reserve the main thread samples is positive and stays there"
        );

        // Walk the silence forward one position-callback period at a time and find the first tick
        // at which the main thread holds the clock. That tick is also what the worker's
        // `StallGuard` reads as `terminal_hold_started`, so it is when the oversized fetch is
        // abandoned and a cheaper rung becomes decidable.
        let mut held_at_ms: Option<u32> = None;
        let mut silence_ms = 0u32;
        while silence_ms <= 82_682 {
            let stopped = native_clock_stopped(Some(silence_ms), buffered_ms);
            if should_begin_hls_rebuffer(false, -1, 5_668, buffered_ms, stopped) {
                held_at_ms = Some(silence_ms);
                break;
            }
            silence_ms += POSITION_PERIOD_MS;
        }
        assert!(
            held_at_ms.is_some_and(|ms| ms <= 1_500),
            "a presentation clock that has physically stopped is the terminal reserve boundary:              expected the hold within 1500 ms of the last position callback, got {held_at_ms:?}              (the device waited 82682 ms for the fetch to complete instead)"
        );
    }

    #[test]
    fn segmented_hls_primes_both_lanes_even_without_a_seek() {
        assert!(prime_before_play(false, true));
        assert!(prime_before_play(true, true));
        assert!(prime_before_play(true, false));
        assert!(!prime_before_play(false, false));
    }

    #[test]
    fn boundary_reserves_are_resume_gates_not_mid_acquisition_pause_triggers() {
        assert!(
            !should_begin_hls_rebuffer(false, -1, 3_000, Some(2_999), false),
            "a full acquisition runway cannot be compared to a partially spent buffer mid-fetch",
        );
        assert!(
            should_begin_hls_rebuffer(true, -1, 3_000, Some(2_999), false),
            "the in-flight fetch may still request a controlled hold from its live progress",
        );
        assert!(
            !should_begin_hls_rebuffer(false, 5_303, 5_303, Some(5_167), false),
            "a bounded candidate that reaches its deadline must finish instead of latching a \
             multi-second playback hold at the retrospective replay balance",
        );
        assert!(
            should_begin_hls_rebuffer(false, -1, 3_000, Some(0), false),
            "physical depletion must hold the clock even when a fetch has no measurable prefix",
        );
        assert!(
            should_begin_hls_rebuffer(false, -1, 3_000, Some(258), true),
            "a presentation clock that has stopped is that same physical depletion, observed \
             where the content-time arithmetic cannot reach zero",
        );
    }

    /// The stopped-clock observation is one-sided: it must fire on the device's remnant and must
    /// not fire on a healthy reserve whose callback is merely late.
    #[test]
    fn only_a_reserve_the_stopped_clock_has_outlasted_is_a_terminal_boundary() {
        assert_eq!(NATIVE_CLOCK_STOPPED_MS, 1_005, "five 201 ms position periods");
        assert!(
            !native_clock_stopped(Some(NATIVE_CLOCK_STOPPED_MS - 1), Some(258)),
            "four and a bit periods of silence is jitter, not a stopped clock",
        );
        assert!(
            native_clock_stopped(Some(NATIVE_CLOCK_STOPPED_MS), Some(258)),
            "the device's remnant is smaller than the silence that has already outlasted it",
        );
        assert!(
            !native_clock_stopped(Some(1_200), Some(5_668)),
            "a stream holding 5.6 s of reserve is not starving because one callback was late",
        );
        assert!(
            native_clock_stopped(Some(5_668), Some(5_668)),
            "silence long enough to have presented the whole reserve is the same boundary",
        );
        assert!(
            !native_clock_stopped(None, Some(0)),
            "an epoch with no position callback yet has nothing to call stopped",
        );
        assert!(
            !native_clock_stopped(Some(82_682), None),
            "an unknowable reserve holds nothing, exactly as it arms no StallGuard",
        );
    }

    /// A seek and a session reset both zero `dg_vpres_at`, so the silence must read as absent
    /// rather than as a gap the size of the seek.
    #[test]
    fn a_seek_leaves_no_silence_observation_behind() {
        let _serial = plx_base::testlock::serial();
        let restore = FEED_SHARED.dg_vpres_at.swap(0, Relaxed);
        assert_eq!(
            native_clock_silence_ms(),
            None,
            "a cleared presentation stamp is no observation, not an infinite one",
        );
        // And a deliberate hold floors the observation at the moment it was restarted, so a long
        // user Pause cannot be read as a stall on the first tick after Resume.
        FEED_SHARED.dg_vpres_at.store(1, Relaxed);
        CLOCK_WATCH_SINCE_MS.store(super::super::vclock_ms(), Relaxed);
        assert!(
            native_clock_silence_ms().is_some_and(|ms| ms < NATIVE_CLOCK_STOPPED_MS),
            "a just-restarted watch cannot already be a stopped clock",
        );
        CLOCK_WATCH_SINCE_MS.store(0, Relaxed);
        FEED_SHARED.dg_vpres_at.store(restore, Relaxed);
    }

    #[test]
    fn media_completed_after_a_hold_request_makes_that_request_stale() {
        assert!(rebuffer_request_is_current(true, 10_000, 10_000));
        assert!(
            !rebuffer_request_is_current(true, 10_000, 12_000),
            "a completed and feedable segment removed the underflow the old request described",
        );
        assert!(!rebuffer_request_is_current(false, 10_000, 10_000));
    }

    /// Run one claim landing's seek settlement the way the tails do: the viewer pressed `pressed`
    /// (if anything) during the flight, the claim was built with `pending_seek`, then the reload
    /// resets the pending seek. Returns what was carried, the pending target after the re-arm, and
    /// whether the reducer still owed the seek intent. The feed's copy of the target is the
    /// readout's at every step.
    fn run_carry(pending_seek: i64, pressed: Option<i64>) -> (Option<i64>, i64, bool) {
        let _serial = plx_base::testlock::serial();
        FEED_TX.reset();
        crate::route::reject_user_seek();
        let mut ps = crate::route::PlaybackSession::IDLE;
        let mut pa = crate::player::adapter::PlayerAdapter::new_fed(unsafe {
            plx_base::task::MainThread::assume()
        });
        if let Some(target) = pressed {
            crate::player::request_seek(&mut ps, &mut pa, target);
        }
        assert_eq!(FEED_TX.seek_to_ns.load(Relaxed), ps.readout().pending_seek().unwrap_or(-1));
        let carried = commit_or_carry_seek(&ps, pending_seek);
        // The reload's teardown: the feed's transport reset and the readout's session reset.
        FEED_TX.reset_for_reload();
        ps.step_readout(crate::player::readout::ReadoutEvent::SessionReset);
        assert_eq!(ps.readout().pending_seek(), None, "the reload cleared what it was asked to");
        rearm_carried_seek(&mut ps, &mut pa, carried);
        let landed = ps.readout().pending_seek().unwrap_or(-1);
        assert_eq!(FEED_TX.seek_to_ns.load(Relaxed), landed, "the feed's gate is armed with the carried target");
        let still_owed = crate::route::commit_user_seek();
        FEED_TX.reset();
        crate::route::reject_user_seek();
        (carried, landed, still_owed)
    }

    /// Finding: a seek pressed during a claim's flight, when NO seek was pending at claim time
    /// (`pending_seek < 0`), was dropped — `commit_or_carry_seek` returned `None` for every
    /// negative `pending_seek`, and the reload's `FEED_TX.reset_for_reload()` then cleared the press.
    #[test]
    fn a_seek_pressed_mid_flight_lands_after_the_reload_when_none_was_pending_at_claim_time() {
        let target = 40_000_000_000;
        let (_, landed, intent_still_owed) = run_carry(-1, Some(target));
        assert_eq!(landed, target, "the mid-flight seek was cleared by the reload instead of carried");
        assert!(intent_still_owed, "the seek intent stays owed for the pump's own seek branch to commit");
    }

    /// A seek pressed AFTER the claim captured an older one supersedes it: carried, not committed.
    #[test]
    fn a_fresher_seek_than_the_claims_is_carried_past_the_reload() {
        let (_, landed, owed) = run_carry(30_000_000_000, Some(50_000_000_000));
        assert_eq!(landed, 50_000_000_000);
        assert!(owed);
    }

    /// The exact-match path: the claim's own seek is still the newest, so the reload crosses it and
    /// the intent is committed (settled) rather than carried.
    #[test]
    fn the_claims_own_seek_is_committed_not_carried() {
        let target = 30_000_000_000;
        let (carried, landed, still_owed) = run_carry(target, Some(target));
        assert_eq!(carried, None);
        assert_eq!(landed, -1, "nothing is re-armed: the reload lands AT the claim's target");
        assert!(!still_owed, "the exact-match commit settled the intent");
    }

    /// No seek anywhere: nothing to carry, nothing to commit.
    #[test]
    fn no_seek_at_all_carries_nothing() {
        let (carried, _, still_owed) = run_carry(-1, None);
        assert_eq!(carried, None);
        assert!(!still_owed);
    }
}

/// **The pump, driving the flight.** A transcode seek and an `AdaptiveReload` claim both rebuild
/// the physical encoder through PMS. These tests run the REAL `pump` inside a `FrameScope` against
/// the hostsim Engine and the loopback server of `route::flight_rig`, and grade what the frame
/// thread did: a frame that makes a PMS round trip is a frame the viewer waits on.
#[cfg(all(test, feature = "hostsim"))]
mod flight_tests {
    use std::sync::atomic::Ordering::Relaxed;
    use super::*;
    use plx_play_feed::shared::FEED_TX;
    use crate::player::{begin_resume, land_resume, suspend_bufferfeed, ResumeOutcome, ResumeStart};
    use crate::route::flight_rig::FlightRig;

    const SEEK_TARGET_NS: i64 = 120_000_000_000;

    struct Rig {
        ps: crate::route::PlaybackSession,
        pa: super::super::adapter::PlayerAdapter,
        flight: FlightRig,
        now: u32,
    }

    impl Rig {
        /// A playing transcode `rig-1` on a loopback server whose `/decision` takes `delay`, and
        /// an Engine streaming it.
        fn new(delay_ms: u64) -> Rig {
            let mut ps = crate::route::PlaybackSession::IDLE;
            let flight = FlightRig::start(&mut ps, Duration::from_millis(delay_ms));
            Rig::on(ps, flight)
        }

        /// A real host object with a pending Rust Load-return gate. The host emits
        /// `loadCompleted` inside Load; this leaves the callback visible while the pump must
        /// still wait for the worker's returned receipt.
        fn new_load_return_pending(delay_ms: u64) -> Rig {
            let mut ps = crate::route::PlaybackSession::IDLE;
            let flight = FlightRig::start(&mut ps, Duration::from_millis(delay_ms));
            Rig::on_with_load_return(ps, flight, false)
        }

        /// [`Rig::new`] on a direct play of the Part, no transcode session (`FlightRig::start_direct`).
        fn direct(delay_ms: u64) -> Rig {
            let mut ps = crate::route::PlaybackSession::IDLE;
            let flight = FlightRig::start_direct(&mut ps, Duration::from_millis(delay_ms));
            Rig::on(ps, flight)
        }

        /// [`Rig::direct`] a cold Auto Original: the watched source that never opened.
        fn direct_auto_original(delay_ms: u64) -> Rig {
            let mut ps = crate::route::PlaybackSession::IDLE;
            let flight = FlightRig::start_direct_auto_original(&mut ps, Duration::from_millis(delay_ms));
            FlightRig::set_bootstrap_rung(&mut ps, crate::abr::Rung::P720Low);
            Rig::on(ps, flight)
        }

        /// The Engine streaming the route `flight` installed in `ps`.
        fn on(ps: crate::route::PlaybackSession, flight: FlightRig) -> Rig {
            Rig::on_with_load_return(ps, flight, true)
        }

        fn on_with_load_return(mut ps: crate::route::PlaybackSession, flight: FlightRig,
            load_returned: bool) -> Rig {
            crate::player::reset_session_for_test();
            FEED_TX.reset();
            super::super::claim_hold::clear();
            plx_play_feed::ffi_host::reset_native_lifecycle_for_test();
            plx_play_feed::ffi_host::force_clocksink_for_test(true);
            let mut pa = super::super::adapter::PlayerAdapter::new_probed(unsafe { MainThread::assume() });
            let epoch = FEED_SHARED.begin_native_session().expect("native session");
            // The installed Engine represents a completed Load. Give the host sink that same
            // object and callback epoch so a later rollback must prove its real Unload/D1.
            assert_eq!(unsafe { crate::player::test_physical::sink().load(c"{}".as_ptr(), epoch) }, 1);
            if load_returned {
                assert!(FEED_SHARED.mark_native_load_returned(epoch, None).is_some());
            }
            let engine = super::super::engine::streaming_engine_for_test(epoch);
            assert!(ps.qualify_installed_feed_offer(crate::route::CandidatePurpose::NewCandidate));
            let offer = ps.planned_feed_offer().expect("installed Feed recipe");
            assert!(ps.backend_offer_admitted(plx_playback_contract::BackendKind::Feed, offer));
            ps.begin_attempt_view(crate::route::RouteStartAttempt::fixture(), super::super::attempt_view::Backend::Feed);
            pa.install(engine);
            FEED_SHARED.duration_ns.store(7_200_000_000_000, Relaxed);
            FEED_SHARED.playpos_ns.store(60_000_000_000, Relaxed);
            Rig { ps, pa, flight, now: 1_000 }
        }

        /// One frame, exactly as the run loop drives it: inside a `FrameScope`, so a PMS call
        /// without an `allow_blocking` panics.
        fn frame(&mut self) {
            let _frame = plx_base::task::FrameScope::enter();
            self.now += 16;
            // The app's frame drain owns a rollback whose trial Load was refused before
            // an Engine existed. It runs before the physical pump on each app frame.
            drain_engineless_rollback(&mut self.ps, &mut self.pa);
            pump(&mut self.ps, &mut self.pa, self.now);
            // Then the persistent common owner's poll (`app::run`'s order), which advances exact
            // startup controls, including the reloaded stream's first Play. Without it a landing's
            // Play never reaches the sink and the feed gate stays shut.
            let at = self.pa.backend_tick();
            let _ = self.pa.backend_poll_session_at(&mut self.ps, at);
        }

        /// Frames until `done`, bounded: the worker lands on its own thread.
        fn frames_until(&mut self, what: &str, done: impl Fn(&Rig) -> bool) {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while !done(self) {
                assert!(std::time::Instant::now() < deadline,
                    "timed out waiting for {what}: phase={}, decisions={}, answered={}, live={}, stage={:?}, recovery={}, state={:?}",
                    crate::route::control_phase_label(), self.flight.decisions(),
                    self.flight.answered(), self.pa.is_live(), self.ps.readout().raw().load.stage.map(|stage| stage as u8),
                    crate::route::recovery_flight_outstanding(), crate::player::state(&self.ps));
                std::thread::sleep(Duration::from_millis(2));
                self.frame();
            }
        }

        fn recovery_diag(&self) -> String {
            format!(
                "state={:?} pb={} route={} offer={:?} planned={:?} allowed={} live={} \
                 active={:?} backend={:?} feed_stage={:?} native_releasing={} \
                 native_abandoned={} start_refused={:?} engine_failure={:?} play_refused={} \
                 resolution_failed={} load_failed={}",
                crate::player::state(&self.ps), self.ps.readout().state() as u8,
                crate::route::control_phase_label(), self.ps.backend_offer(),
                self.ps.planned_feed_offer(), self.ps.backend_planned_feed_allowed(),
                self.pa.is_live(), self.pa.active_feed_attempt(), self.pa.backend_snapshot(),
                self.ps.readout().raw().load.stage.map(|stage| stage as u8), FEED_SHARED.native_releasing(),
                FEED_SHARED.native_load_abandoned(), self.ps.native_start_refused,
                crate::route::engine_failure(&self.ps), crate::route::play_refused(&self.ps),
                crate::route::play_resolution_failed(&self.ps), FEED_SHARED.load_failed.load(Relaxed),
            )
        }
    }

    impl Drop for Rig {
        fn drop(&mut self) {
            // A landed reload left a real Engine whose demuxer holds raw pointers into its AU
            // queues; dropping the adapter with it installed freed them under that thread. Join it
            // the way an app switch does, which neither scrobbles nor stops a server transcode.
            if self.pa.is_live() {
                suspend_bufferfeed(&mut self.ps, &mut self.pa);
            }
            if self.pa.has_pending_release() {
                super::super::route_start::finish_release_for_test(&mut self.pa);
            }
            super::super::claim_hold::clear();
            plx_play_feed::ffi_host::reset_native_lifecycle_for_test();
            plx_play_feed::ffi_host::force_clocksink_for_test(false);
            plx_play_feed::ffi_host::force_object_ready_for_test(false);
            FEED_SHARED.test_force_native_idle();
            crate::player::reset_session_for_test();
            FEED_TX.reset();
            plx_play_feed::native_audio::reset();
            crate::route::reset_player_control_for_test(crate::route::idle_session_for_test());
        }
    }

    /// A seek tap on a transcode hands the PMS half to a worker: the frame returns without waiting
    /// for the server, a flight is outstanding, and the seek lands on a later frame.
    ///
    /// "Without waiting" is graded CAUSALLY: the rig is told to hold every answer, so
    /// when the frame returns no answer has been written, and a frame that had made the round trip
    /// itself could not have returned at all until the server's safety release. Nothing here depends
    /// on how long the frame took. The request LOG is not graded: the rig logs a `/decision` the
    /// moment it arrives and the worker may legitimately have sent it by then (the earlier
    /// `decisions() == 0` check failed in 2 of 4 gate runs of two `make check`s side by side).
    #[test]
    fn a_transcode_seek_frame_makes_no_pms_call_and_the_worker_lands_it_later() {
        let mut rig = Rig::new(150);
        rig.frame();
        assert_eq!(rig.flight.decisions(), 0, "sanity: an idle frame asks PMS nothing");
        let before = crate::route::transcode_session(&rig.ps);

        rig.flight.hold_answers();
        crate::player::request_seek(&mut rig.ps, &mut rig.pa, SEEK_TARGET_NS);
        rig.frame();
        assert_eq!(
            rig.flight.answered(),
            0,
            "the seek frame returned only after the server answered: a PMS round trip on the frame \
             thread: {:?}",
            rig.flight.requests()
        );
        assert!(crate::route::flight_outstanding(), "the rebuild must be a flight the pump waits on");
        assert_eq!(
            crate::player::state(&rig.ps),
            crate::player::PlaybackState::Seeking,
            "the HUD shows the Seeking spinner for the whole flight"
        );
        assert!(super::super::claim_hold::active(), "the old picture is held under the spinner");
        assert!(FEED_TX.paused.load(Acquire), "the hold paused the stream, as a direct-play seek does");

        rig.flight.release_answers();
        rig.frames_until("the seek's landing", |_| !crate::route::flight_outstanding());
        assert_eq!(rig.flight.decisions(), 1, "exactly one /decision for the seek");
        assert_ne!(crate::route::transcode_session(&rig.ps), before, "the landing installed the replacement encoder");
        assert_eq!(FEED_TX.seek_to_ns.load(Relaxed), -1, "the seek was consumed, not re-armed");
        assert_eq!(rig.ps.readout().pending_seek(), None, "and the player's own target went with the feed's");
        rig.flight.wait_for("the replaced encoder's stop", |f| f.stopped().iter().any(|s| s == "rig-1"));
    }

    /// Field report (nightly 0.10.0-nightly-20261108, webOS 4, Boost Dialog remux, Segment seek
    /// mode): a scrub on a PLAYING transcode flies with the claim hold engaged, the landing reloads
    /// at the target (`reload_transcode: fresh Load`), the hold gives play back while the new Load
    /// is still in flight (`player: user Resume accepted; physical Play remains fenced`), and the
    /// fresh stream reaches `SMP loadCompleted`, fills both lanes to BufferFull, and never gets
    /// `SMP Play`: the clock stays frozen at the target until the viewer backs out.
    ///
    /// Scrubs `rig`'s PLAYING transcode to its landing, then until the RELOADED Engine reports
    /// loadCompleted. Answers the common runner's startup goal right after the landing (for the
    /// failure message) and the sink's Play count at that point.
    fn scrub_until_reloaded(rig: &mut Rig) -> (Option<bool>, u64) {
        FEED_SHARED.frames.store(1, Relaxed);
        assert!(!FEED_TX.paused.load(Acquire), "sanity: the transcode is playing");
        let old_epoch = engine_epoch(rig);

        crate::player::request_seek(&mut rig.ps, &mut rig.pa, SEEK_TARGET_NS);
        rig.frame();
        assert!(crate::route::flight_outstanding(), "the scrub must fly as a transcode seek");
        assert!(super::super::claim_hold::owns_pause(), "a playing stream: the claim hold took the Pause");

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while crate::route::flight_outstanding() {
            assert!(std::time::Instant::now() < deadline, "timed out waiting for the seek's landing");
            std::thread::sleep(Duration::from_millis(2));
            rig.frame();
        }
        // The hold owed the viewer a Play back; the field's give-back read `player: user Resume
        // accepted; physical Play remains fenced`. The physical gate may stay shut until the
        // startup Play; the common runner's startup goal is what decides whether that Play comes.
        let goal_after_landing = rig.pa.backend_viewer_paused();
        let plays = plx_play_feed::ffi_host::play_calls_for_test();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let loaded = rig.pa.engine().is_some_and(|eng|
                Some(eng.native_epoch) != old_epoch && eng.stage >= Stage::Playing);
            if loaded { break; }
            assert!(std::time::Instant::now() < deadline,
                "timed out waiting for the reloaded Engine's loadCompleted: {}", rig.recovery_diag());
            std::thread::sleep(Duration::from_millis(2));
            rig.frame();
        }
        (goal_after_landing, plays)
    }

    /// Both lanes of the reloaded Engine filled past the prime thresholds (`PRIME_NS` of video,
    /// `PRIME_AUDIO_NS` of audio): what the field's "both AU lanes full" was, pushed into the
    /// queues the pump's feed lanes drain.
    fn fill_both_lanes(rig: &mut Rig) {
        const V_STEP_NS: i64 = 41_708_333; // 23.976 fps
        const A_STEP_NS: i64 = 32_000_000; // 1536 samples at 48 kHz
        let mut eng = rig.pa.engine().expect("the reloaded Engine");
        let base = eng.seek_base_pts;
        let au = [0u8; 64];
        let video = &mut **eng.aq_video.as_mut().expect("the video lane") as *mut plx_play_feed::aq::AuQueue;
        for i in 0..48i64 {
            plx_play_feed::aq::aq_push(video, au.as_ptr(), au.len() as std::ffi::c_int,
                base + i * V_STEP_NS, std::ffi::c_int::from(i == 0), 1);
        }
        let audio = &mut **eng.aq_audio.as_mut().expect("the audio lane") as *mut plx_play_feed::aq::AuQueue;
        for i in 0..64i64 {
            plx_play_feed::aq::aq_push(audio, au.as_ptr(), au.len() as std::ffi::c_int,
                base + i * A_STEP_NS, 1, 2);
        }
    }

    /// The post-scrub landing's whole contract, graded on the physical sink: the common startup
    /// runner commands the reloaded stream's first Play (`primed` is whether that Play waits for
    /// the prime proof or goes at once), it reaches the sink, the feed gate is open and the
    /// clock runs and advances.
    fn assert_scrub_reload_plays(rig: &mut Rig, primed: bool) {
        let (goal_after_landing, plays) = scrub_until_reloaded(rig);
        {
            let eng = rig.pa.engine().expect("the reloaded Engine");
            assert_eq!(eng.startup_immediate_play, !primed,
                "PRECONDITION: the reloaded start is {} (startup_immediate_play={}, prime_play={}, rebase_pending={})",
                if primed { "primed before Play" } else { "an immediate-Play start" },
                eng.startup_immediate_play, eng.prime_play, eng.rebase_pending);
        }
        fill_both_lanes(rig);
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while plx_play_feed::ffi_host::play_calls_for_test() <= plays {
            let (commanded, prime, immediate, user_held) = {
                let eng = rig.pa.engine().expect("the reloaded Engine");
                (eng.startup_play_commanded, eng.prime_play, eng.startup_immediate_play,
                    eng.startup_user_held)
            };
            assert!(std::time::Instant::now() < deadline,
                "the reloaded stream's first Play never reached the sink (startup_play_commanded={commanded}, \
                 prime_play={prime}, immediate={immediate}, startup_user_held={user_held}, runner's startup \
                 goal after the landing paused={goal_after_landing:?}, now {:?}): no `SMP Play` can ever \
                 follow, and try_prime returns early forever",
                rig.pa.backend_viewer_paused());
            std::thread::sleep(Duration::from_millis(2));
            rig.frame();
        }
        assert!(!FEED_TX.paused.load(Acquire), "the feed gate is open once the stream plays");
        let (before, running) = plx_play_feed::ffi_host::clock_state_for_test();
        assert!(running, "the sink clock runs after the first Play");
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            rig.frame();
            if plx_play_feed::ffi_host::clock_state_for_test().0 > before { break; }
            assert!(std::time::Instant::now() < deadline,
                "the sink clock never advanced past {before}ns after the first Play");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// The progressive (non-rebased) transcode: the first Play is an immediate one.
    #[test]
    fn a_playing_transcode_seek_landing_issues_play_on_the_reloaded_stream() {
        let mut rig = Rig::new(60);
        assert_scrub_reload_plays(&mut rig, false);
    }

    /// The same scrub when the reloaded start must prime before Play (`prime_before_play`: a
    /// rebased or segmented-HLS start), where the runner's Play goes through
    /// `FeedTransportAction::ResumeInitial` and the prime proof releases the sink's Play.
    #[test]
    fn a_playing_primed_transcode_seek_landing_issues_play_on_the_reloaded_stream() {
        let mut rig = hls_rig(60);
        assert_scrub_reload_plays(&mut rig, true);
    }

    /// The track-memory write-back is sent from the pump's decoded-frame branch and from nowhere
    /// else (W6): a frame before the first decoded picture leaves it parked, the first frame
    /// with `frames > 0` takes it. The session is the rig's transcode, so the send drops the value
    /// rather than queueing a PUT; what is graded here is only WHEN the pump takes it.
    #[test]
    fn the_first_decoded_frame_takes_the_parked_write_back_and_an_earlier_frame_does_not() {
        let mut rig = Rig::new(150);
        crate::route::park_write_back_for_test(&mut rig.ps);
        assert_eq!(FEED_SHARED.frames.load(Relaxed), 0, "sanity: nothing decoded yet");
        rig.frame();
        assert!(crate::route::write_back_parked(&rig.ps), "no decoded frame yet: the write stays parked");

        FEED_SHARED.frames.store(1, Relaxed);
        rig.frame();
        assert!(!crate::route::write_back_parked(&rig.ps), "the first decoded frame took the parked write");
    }

    /// A Home press while a seek flight is out saves the SEEK TARGET (what `app::intended_pos`
    /// snapshots for the lifecycle's `Park`), not the playhead the viewer just left: the published
    /// `playpos_ns` still reads the pre-seek 60 s for the whole flight.
    #[test]
    fn a_suspend_snapshot_during_a_seek_flight_is_the_seek_target() {
        let mut rig = Rig::new(150);
        crate::player::request_seek(&mut rig.ps, &mut rig.pa, SEEK_TARGET_NS);
        rig.frame();
        assert!(crate::route::flight_outstanding(), "the seek must be flying");
        assert_eq!(FEED_SHARED.playpos_ns.load(Relaxed), 60_000_000_000, "sanity: the published position is the old one");
        assert_eq!(crate::player::intended_pos_ns(&rig.ps), SEEK_TARGET_NS);
    }

    /// `quality: Auto picked — retaining live HLS and refreshing its adaptive contract` queues an
    /// `AdaptiveReload`; on a transcode the arm serving it plans a rebase inside its own claim
    /// (`ControlPhase::Applying`), so the claim's own flight must not be what refuses it.
    #[test]
    fn an_adaptive_reload_on_a_transcode_is_served_not_rejected_by_its_own_claim() {
        let mut rig = Rig::new(60);
        let before = crate::route::transcode_session(&rig.ps);
        crate::player::request_adaptive_reload(&rig.ps);

        rig.frame();
        assert!(
            crate::route::flight_outstanding(),
            "the claim was settled on the spot — it refused itself without asking PMS: {:?}",
            rig.flight.requests()
        );
        assert!(super::super::claim_hold::active(), "the reload lands at the claim offset, so the stream holds there");

        rig.frames_until("the adaptive reload to settle", |r| {
            !crate::route::flight_outstanding() && r.flight.decisions() > 0
        });
        assert_eq!(rig.flight.decisions(), 1, "the arm never asked PMS to rebuild: the claim refused itself");
        assert_ne!(crate::route::transcode_session(&rig.ps), before, "the rebuilt encoder is the route's now");
    }

    /// What a fresh Engine after a reload must reach while the viewer is paused: the paused-seek
    /// preroll open (so the feed gate lets the keyframe through and the prime may Play), then, once
    /// a frame is presented, the film re-paused. The rig's Engine is the one the reload installed.
    fn assert_paused_reload_primes_a_first_frame(rig: &mut Rig) {
        // The reload zeroed the session: no picture yet, viewer still paused.
        FEED_SHARED.frames.store(0, Relaxed);
        // The fresh Engine a reload installs owes its prime (`start_bufferfeed` arms it); the rig's
        // fixture Engine is not one.
        rig.pa.engine().expect("engine").prime_play = true;
        assert!(FEED_TX.paused.load(Acquire), "the viewer is still paused after the reload");
        rig.frame();
        rig.frame();
        assert!(
            FEED_TX.feed_allowed(),
            "a reload under a viewer Pause left the feed gate shut on a session with no picture: the \
             keyframe is never fed and the HUD shows Buffering forever"
        );
        FEED_SHARED.frames.store(1, Relaxed);
        rig.frame();
        rig.frame();
        assert!(!FEED_TX.seek_preroll_active(), "the preroll closed once the first frame was presented");
        assert!(FEED_TX.paused.load(Acquire), "the film ends paused");
    }

    /// The viewer is paused on a picture; Boost Dialog / Normalize Loudness requests an adaptive
    /// reload; the fresh session has no picture and must be primed to show one.
    #[test]
    fn a_paused_adaptive_reload_primes_a_first_frame_and_ends_paused() {
        let mut rig = Rig::new(60);
        FEED_SHARED.frames.store(1, Relaxed);
        FEED_SHARED.reset_hls_clock_for_test();
        assert!(crate::player::pause(&mut rig.ps, &mut rig.pa), "viewer Pause accepted");
        crate::player::request_adaptive_reload(&rig.ps);
        rig.frames_until("the adaptive reload to settle", |r| {
            !crate::route::flight_outstanding() && r.flight.decisions() > 0
        });
        rig.frame();
        assert_paused_reload_primes_a_first_frame(&mut rig);
    }

    /// A Pause pressed while the claim's flight is out: the claim hold took the Pause first, the
    /// viewer's press forgets its restore, and the landing reloads a stream nobody owes a Play.
    #[test]
    fn a_pause_pressed_during_a_claim_flight_primes_the_reloaded_stream_a_first_frame() {
        let mut rig = Rig::new(150);
        FEED_SHARED.frames.store(1, Relaxed);
        rig.flight.hold_answers();
        crate::player::request_adaptive_reload(&rig.ps);
        rig.frame();
        assert!(crate::route::flight_outstanding(), "the claim must be in flight");
        assert!(super::super::claim_hold::owns_pause(), "a playing stream: the hold took the Pause");
        // Negative: the hold's own pause is given back by the hold, never primed over.
        FEED_SHARED.frames.store(0, Relaxed);
        rig.frame();
        assert!(!FEED_TX.seek_preroll_active(), "a claim hold's pause opened a preroll");
        FEED_SHARED.frames.store(1, Relaxed);
        assert!(crate::player::transport::set_transport_paused(&mut rig.ps, &mut rig.pa, true), "the viewer's Pause press");
        assert!(!super::super::claim_hold::owns_pause(), "the viewer's press outranks the hold");
        rig.flight.release_answers();
        rig.frames_until("the adaptive reload to settle", |r| {
            !crate::route::flight_outstanding() && r.flight.decisions() > 0
        });
        rig.frame();
        assert_paused_reload_primes_a_first_frame(&mut rig);
    }

    /// A rig whose route is a live fixed-rung HLS transcode under an applied Auto contract.
    fn hls_rig(delay_ms: u64) -> Rig {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let flight = FlightRig::start_hls(&mut ps, Duration::from_millis(delay_ms));
        crate::player::reset_session_for_test();
        FEED_TX.reset();
        super::super::claim_hold::clear();
        plx_play_feed::ffi_host::reset_native_lifecycle_for_test();
        plx_play_feed::ffi_host::force_clocksink_for_test(true);
        let mut pa = super::super::adapter::PlayerAdapter::new_probed(unsafe { MainThread::assume() });
        let epoch = FEED_SHARED.begin_native_session().expect("native session");
        assert_eq!(unsafe { crate::player::test_physical::sink().load(c"{}".as_ptr(), epoch) }, 1);
        assert!(FEED_SHARED.mark_native_load_returned(epoch, None).is_some());
        ps.begin_attempt_view(crate::route::RouteStartAttempt::fixture(), super::super::attempt_view::Backend::Feed);
        pa.install(super::super::engine::streaming_engine_for_test(epoch));
        FEED_SHARED.duration_ns.store(7_200_000_000_000, Relaxed);
        FEED_SHARED.playpos_ns.store(60_000_000_000, Relaxed);
        Rig { ps, pa, flight, now: 1_000 }
    }

    /// The native session the Engine in the pump's slot is running (a reload replaces it).
    fn engine_epoch(rig: &mut Rig) -> Option<u32> {
        rig.pa.engine().map(|e| e.native_epoch)
    }

    /// A manual Original pick on a transcode hands the recovery's Part admission, selection PUT and
    /// replacement `/decision` to a worker and returns having asked the server NOTHING; the landing
    /// installs through the same drain as an enhancement release and reloads onto the Original trial.
    #[test]
    fn a_manual_original_claim_frame_makes_no_pms_call_and_the_worker_lands_it_later() {
        let mut rig = Rig::new(150);
        FlightRig::offer_original(&mut rig.ps, false);
        rig.frame();
        let answered = rig.flight.answered();
        let epoch = engine_epoch(&mut rig);

        rig.flight.hold_answers();
        FlightRig::queue_manual_original(&rig.ps, false);
        rig.frame();
        assert_eq!(
            rig.flight.answered(),
            answered,
            "the claiming frame waited for a PMS answer on the frame thread: {:?}",
            rig.flight.requests()
        );
        assert!(crate::route::flight_outstanding(), "the recovery's PMS half must be a flight");
        assert!(super::super::claim_hold::active(), "a user's claim holds the picture at its offset");
        assert_eq!(engine_epoch(&mut rig), epoch, "nothing reloads before the landing");
        rig.flight.release_answers();

        rig.frames_until("the recovery's landing", |_| !crate::route::flight_outstanding());
        assert_eq!(rig.flight.decisions(), 1, "exactly one replacement /decision");
        assert!(crate::route::original_recovery_pending(), "the drain installed the Original trial and its way back");
        assert_ne!(crate::route::transcode_session(&rig.ps), "rig-1", "the trial names the replacement encoder");
        assert_ne!(engine_epoch(&mut rig), epoch, "the landing reloaded onto the trial");
        assert!(
            !rig.flight.stopped().iter().any(|s| s == "rig-1"),
            "the encoder being left is the trial's way back until frames commit it: {:?}",
            rig.flight.stopped()
        );
    }

    /// A refused Original keeps the stream on screen — and a track pick merged under the claim
    /// (`displaced_pick`) is still owed its own reload, which also runs on the worker.
    #[test]
    fn a_refused_manual_original_pays_the_displaced_picks_reload_off_the_frame() {
        let mut rig = Rig::new(40);
        FlightRig::offer_original(&mut rig.ps, false);
        rig.flight.refuse_decisions(1);
        rig.frame();
        let answered = rig.flight.answered();

        rig.flight.hold_answers();
        FlightRig::queue_manual_original(&rig.ps, true);
        rig.frame();
        assert_eq!(rig.flight.answered(), answered, "{:?}", rig.flight.requests());
        assert!(crate::route::flight_outstanding());
        rig.flight.release_answers();

        rig.frames_until("the owed reload's landing", |r| {
            !crate::route::flight_outstanding() && r.flight.decisions() == 2
        });
        assert!(!crate::route::original_recovery_pending(), "Original was refused: no trial");
        assert_ne!(crate::route::transcode_session(&rig.ps), "rig-1", "the displaced pick's rebuild is the route now");
        rig.flight.wait_for("the replaced encoder's stop", |f| f.stopped().iter().any(|s| s == "rig-1"));
    }

    /// A refused Original with nothing owed: the stream the viewer is watching is untouched, the
    /// reducer is Stable, the hold gave play back and the refused replacement was stopped.
    #[test]
    fn a_refused_manual_original_keeps_the_live_stream_and_gives_play_back() {
        let mut rig = Rig::new(40);
        FlightRig::offer_original(&mut rig.ps, false);
        rig.flight.refuse_decisions(1);
        rig.frame();
        let epoch = engine_epoch(&mut rig);

        FlightRig::queue_manual_original(&rig.ps, false);
        rig.frame();
        assert!(super::super::claim_hold::active());
        rig.frames_until("the refusal's landing", |_| !crate::route::flight_outstanding());
        assert_eq!(rig.flight.decisions(), 1, "a refusal with nothing owed asks once");
        assert_eq!(crate::route::transcode_session(&rig.ps), "rig-1");
        assert_eq!(engine_epoch(&mut rig), epoch, "a rejection reloads nothing");
        assert!(!crate::route::original_recovery_pending());
        assert!(!super::super::claim_hold::active() && !FEED_TX.paused.load(Acquire), "the hold gave play back");
        rig.flight.wait_for("the refused replacement's stop", |f| {
            f.stopped().iter().any(|s| s.starts_with("rig-logical-abr-"))
        });
        assert!(!rig.flight.stopped().iter().any(|s| s == "rig-1"));
    }

    /// The automatic HLS-to-Original recovery is the same flight, owned by the automatic claim.
    #[test]
    fn an_automatic_original_recovery_frame_makes_no_pms_call_and_the_worker_lands_it_later() {
        let mut rig = hls_rig(150);
        FlightRig::offer_original(&mut rig.ps, false);
        rig.frame();
        let answered = rig.flight.answered();
        let epoch = engine_epoch(&mut rig);

        rig.flight.hold_answers();
        FlightRig::publish_hls_to_original(60_000_000_000);
        rig.frame();
        assert_eq!(
            rig.flight.answered(),
            answered,
            "the claiming frame waited for a PMS answer on the frame thread: {:?}",
            rig.flight.requests()
        );
        assert!(crate::route::flight_outstanding());
        assert_eq!(engine_epoch(&mut rig), epoch);
        rig.flight.release_answers();

        rig.frames_until("the recovery's landing", |_| !crate::route::flight_outstanding());
        assert_eq!(rig.flight.decisions(), 1);
        assert!(crate::route::original_recovery_pending(), "the drain installed the trial");
        assert_ne!(engine_epoch(&mut rig), epoch, "and reloaded onto it at the action's position");
        assert!(
            !rig.flight.stopped().iter().any(|s| s == "rig-1"),
            "the HLS encoder is the trial's way back: {:?}",
            rig.flight.stopped()
        );
    }

    /// A direct Original needs no PMS at all, so its flight lands within a frame or two and the
    /// stream is never paused for it: an automatic upgrade does not interrupt playback.
    #[test]
    fn an_automatic_direct_original_recovery_flies_without_pausing_the_stream() {
        let mut rig = hls_rig(0);
        FlightRig::offer_original(&mut rig.ps, true);
        rig.frame();
        let epoch = engine_epoch(&mut rig);

        FlightRig::publish_hls_to_original(60_000_000_000);
        rig.frame();
        assert!(!super::super::claim_hold::active(), "no PMS half, no presentation hold");
        assert!(!FEED_TX.paused.load(Acquire), "the stream was not paused for the flight");
        rig.frames_until("the recovery's landing", |_| !crate::route::flight_outstanding());
        assert_eq!(rig.flight.decisions(), 0, "a direct Original asks the server for no replacement");
        assert!(crate::route::original_recovery_pending());
        assert_ne!(engine_epoch(&mut rig), epoch);
        assert!(!FEED_TX.paused.load(Acquire));
    }

    /// A refused automatic recovery leaves the retained HLS route exactly as it was: the producer
    /// stopped to hand the action over, so the pump reopens THAT route (same encoder, a fresh
    /// Engine) rather than arming a trial; nothing else is reloaded, nothing is left held.
    #[test]
    fn a_refused_automatic_original_recovery_reopens_the_retained_hls() {
        let mut rig = hls_rig(40);
        FlightRig::offer_original(&mut rig.ps, false);
        rig.flight.refuse_decisions(1);
        rig.frame();
        let epoch = engine_epoch(&mut rig);

        FlightRig::publish_hls_to_original(60_000_000_000);
        rig.frame();
        rig.frames_until("the refusal's landing", |_| !crate::route::flight_outstanding());
        assert_eq!(rig.flight.decisions(), 1);
        assert_eq!(crate::route::transcode_session(&rig.ps), "rig-1", "the retained HLS is still the route");
        assert!(!crate::route::original_recovery_pending());
        assert_ne!(engine_epoch(&mut rig), epoch, "the producer had stopped, so the HLS Engine was reopened");
        // The reopened Engine's first Play belongs to the common startup runner: the feed gate
        // opens once its Load completes and the runner's Play is accepted, not at the landing.
        rig.frames_until("the reopened stream's first Play opens the feed gate",
            |_| !FEED_TX.paused.load(Acquire));
        assert!(!super::super::claim_hold::active() && !FEED_TX.paused.load(Acquire));
        assert!(!rig.flight.stopped().iter().any(|s| s == "rig-1"));
    }

    /// **A refused automatic recovery is not asked again on the route that refused it.** The
    /// reopen above spawns a fresh HLS worker on the SAME encoder, and its capture
    /// (`route::hls_abr_control`) used to hand it a fresh source gate: on a server that refuses
    /// the Part and the remux every time, the worker probed, asked and was refused again ~10 s
    /// later, a visible reload each round, for the rest of the film. The reopened worker's capture
    /// now carries the refusal, which holds its source gate until the route changes
    /// (`route::decision_audio_enhancement_tests`' encoder-scoped twin grades the release). The
    /// ~10 s cadence itself is the pipeline tier's (the real worker's probe funding); this grades
    /// its only input.
    #[test]
    fn a_refused_automatic_original_recovery_is_not_asked_again_on_the_reopened_hls() {
        let mut rig = hls_rig(40);
        FlightRig::offer_original(&mut rig.ps, false);
        rig.flight.refuse_decisions(1);
        rig.frame();
        let (fresh, _) = crate::route::hls_abr_control(&rig.ps).expect("the live HLS worker's capture");
        assert!(!fresh.original_refused_on_this_route(), "sanity: nothing has been refused yet");

        FlightRig::publish_hls_to_original(60_000_000_000);
        rig.frame();
        rig.frames_until("the refusal's landing", |_| !crate::route::flight_outstanding());
        assert_eq!(crate::route::transcode_session(&rig.ps), "rig-1", "the retained HLS is the route");
        let (reopened, ticket) = crate::route::hls_abr_control(&rig.ps).expect("the reopened HLS worker's capture");
        assert_eq!(ticket.encoder(), "rig-1");
        assert!(
            reopened.original_refused_on_this_route(),
            "the worker reopened on the encoder that refused Original must not ask for it again",
        );
    }

    /// A rig whose route is an Original the Auto watchdog watches (a progressive remux `rig-1`
    /// under an applied Auto contract).
    fn auto_original_rig(delay_ms: u64) -> Rig {
        let mut ps = crate::route::PlaybackSession::IDLE;
        let flight = FlightRig::start_auto_original(&mut ps, Duration::from_millis(delay_ms));
        crate::player::reset_session_for_test();
        FEED_TX.reset();
        super::super::claim_hold::clear();
        plx_play_feed::ffi_host::reset_native_lifecycle_for_test();
        plx_play_feed::ffi_host::force_clocksink_for_test(true);
        let mut pa = super::super::adapter::PlayerAdapter::new_probed(unsafe { MainThread::assume() });
        let epoch = FEED_SHARED.begin_native_session().expect("native session");
        assert_eq!(unsafe { crate::player::test_physical::sink().load(c"{}".as_ptr(), epoch) }, 1);
        assert!(FEED_SHARED.mark_native_load_returned(epoch, None).is_some());
        ps.begin_attempt_view(crate::route::RouteStartAttempt::fixture(), super::super::attempt_view::Backend::Feed);
        pa.install(super::super::engine::streaming_engine_for_test(epoch));
        FEED_SHARED.duration_ns.store(7_200_000_000_000, Relaxed);
        FEED_SHARED.playpos_ns.store(60_000_000_000, Relaxed);
        Rig { ps, pa, flight, now: 1_000 }
    }

    /// The automatic Original-to-HLS fallback is the same flight, owned by the automatic claim, and
    /// the stream is never paused for it: the Original is what plays until the landing replaces it.
    #[test]
    fn an_automatic_hls_fallback_frame_makes_no_pms_call_and_the_worker_lands_it_later() {
        let mut rig = auto_original_rig(150);
        rig.frame();
        let answered = rig.flight.answered();
        let epoch = engine_epoch(&mut rig);

        rig.flight.hold_answers();
        FlightRig::publish_original_to_hls(6_000, 60_000_000_000);
        rig.frame();
        assert_eq!(
            rig.flight.answered(),
            answered,
            "the claiming frame waited for a PMS answer on the frame thread: {:?}",
            rig.flight.requests()
        );
        assert!(crate::route::flight_outstanding(), "the fallback's PMS half must be a flight");
        assert!(!super::super::claim_hold::active(), "the live Original is not held for the flight");
        assert!(!FEED_TX.paused.load(Acquire), "the stream was not paused for the flight");
        assert_eq!(engine_epoch(&mut rig), epoch, "nothing reloads before the landing");
        assert_eq!(crate::route::transcode_session(&rig.ps), "rig-1", "the Original is still the route");
        rig.flight.release_answers();

        rig.frames_until("the fallback's landing", |_| !crate::route::flight_outstanding());
        assert_eq!(rig.flight.decisions(), 1, "exactly one replacement /decision");
        assert_ne!(crate::route::transcode_session(&rig.ps), "rig-1", "the landing named the HLS encoder");
        assert!(crate::route::is_segmented_hls(&rig.ps), "and the route is HLS now");
        assert_ne!(engine_epoch(&mut rig), epoch, "the landing reloaded onto it at the action's position");
        rig.flight.wait_for("the replaced Original's stop", |f| f.stopped().iter().any(|s| s == "rig-1"));
    }

    /// A refused fallback leaves the route as it was and leaks nothing: the replacement the server
    /// registered is stopped, the Original's encoder is not, and nothing is reloaded. The Auto
    /// worker exited to hand the action over, so — exactly as before the flight — the producer's
    /// failure flag is raised for the pump's ordinary recovery.
    #[test]
    fn a_refused_automatic_hls_fallback_keeps_the_original_route_and_leaks_no_encoder() {
        let mut rig = auto_original_rig(40);
        rig.flight.refuse_decisions(1);
        rig.frame();
        let epoch = engine_epoch(&mut rig);

        FlightRig::publish_original_to_hls(6_000, 60_000_000_000);
        rig.frame();
        assert!(crate::route::flight_outstanding());
        rig.frames_until("the refusal's landing", |_| !crate::route::flight_outstanding());
        assert_eq!(rig.flight.decisions(), 1);
        assert_eq!(crate::route::transcode_session(&rig.ps), "rig-1", "the Original is still the route");
        assert!(!crate::route::is_segmented_hls(&rig.ps));
        assert_eq!(engine_epoch(&mut rig), epoch, "a rejection reloads nothing");
        assert!(FEED_SHARED.demux_io_failed.load(Acquire), "the handoff's producer is gone: the pump's recovery owns it");
        assert!(!super::super::claim_hold::active() && !FEED_TX.paused.load(Acquire));
        rig.flight.wait_for("the refused replacement's stop", |f| {
            f.stopped().iter().any(|s| s.starts_with("rig-logical-abr-"))
        });
        assert!(!rig.flight.stopped().iter().any(|s| s == "rig-1"), "{:?}", rig.flight.stopped());
    }

    #[test]
    fn a_stop_from_the_server_ends_the_playback_with_its_reason_and_only_back() {
        let mut rig = Rig::new(120);
        FEED_SHARED.frames.store(10, Relaxed);
        crate::player::note_server_stop(&crate::route::sess(&rig.ps), "rig-1", "stopped by the admin", 10_000);
        rig.frame();
        assert_eq!(crate::player::state(&rig.ps), crate::player::PlaybackState::Error);
        rig.frame();
        assert!(!rig.pa.is_live(), "the Engine is released, with no recovery attempted");
        let shape = crate::player::error_now(&rig.ps);
        assert_eq!(shape.kind, crate::player::FailureKind::ServerStopped);
        assert_eq!(shape.detail, "stopped by the admin");
        assert_eq!(
            crate::player::failure_actions(shape.kind, crate::player::failure_context(&rig.ps)),
            vec![crate::player::FailureAction::Back]
        );
    }

    #[test]
    fn server_stop_releases_a_replacement_engine_before_its_first_picture() {
        let mut rig = failed_original_trial_rig(40);
        assert_eq!(FEED_SHARED.frames.load(Relaxed), 0);
        crate::player::request_seek(&mut rig.ps, &mut rig.pa, SEEK_TARGET_NS);
        crate::player::note_server_stop(&crate::route::sess(&rig.ps), "rig-1", "stopped on reload", 10_000);
        rig.frame();
        assert_eq!(crate::player::state(&rig.ps), crate::player::PlaybackState::Error);
        rig.frame();
        assert!(!rig.pa.is_live(), "the stopped replacement still owns its engine");
        assert_eq!(crate::route::unpresented_resume_ns(&rig.ps), 10_000_000_000);
        assert_eq!(crate::player::error_now(&rig.ps).kind, crate::player::FailureKind::ServerStopped);
    }

    #[test]
    fn server_stop_cannot_return_to_the_readout_after_back() {
        let mut rig = Rig::new(40);
        let playback = crate::route::sess(&rig.ps);
        crate::player::note_server_stop(&playback, "rig-1", "stopped by the admin", 10_000);
        crate::player::stop_bufferfeed(&mut rig.ps, &mut rig.pa);
        assert_eq!(crate::player::state(&rig.ps), crate::player::PlaybackState::Idle);

        // A reporter detached by a reload can still return after Back.
        crate::player::note_server_stop(&playback, "rig-1", "another late reply", 10_000);
        rig.frame();
        assert_eq!(crate::player::state(&rig.ps), crate::player::PlaybackState::Idle);
        crate::route::drain_scrobble();
    }

    #[test]
    fn server_stop_replaces_the_retry_after_the_engine_is_already_released() {
        let mut rig = Rig::new(120);
        FEED_SHARED.frames.store(10, Relaxed);
        FEED_SHARED.demux_io_failed.store(true, Release);
        rig.frame();
        rig.frame();
        assert!(!rig.pa.is_live(), "premise: the interrupted engine was released");
        assert_eq!(crate::player::error_now(&rig.ps).kind, crate::player::FailureKind::PlaybackInterrupted);

        crate::player::note_server_stop(&crate::route::sess(&rig.ps), "rig-1", "late termination", 10_000);
        let shape = crate::player::error_now(&rig.ps);
        assert_eq!(shape.kind, crate::player::FailureKind::ServerStopped);
        assert_eq!(shape.detail, "late termination");
    }

    #[test]
    fn a_stop_from_the_server_during_a_recovery_flight_ends_it_and_reloads_nothing() {
        let mut rig = failed_original_trial_rig(40);
        let epoch = engine_epoch(&mut rig);
        rig.flight.hold_answers();
        FEED_SHARED.load_failed.store(true, Release);
        rig.frame();
        assert!(crate::route::flight_outstanding(), "sanity: the rollback flies");
        let stops = replacement_stops(&rig.flight);

        crate::player::note_server_stop(&crate::route::sess(&rig.ps), "rig-1", "stopped by the admin", 10_000);
        rig.frame();
        assert_eq!(
            crate::player::state(&rig.ps),
            crate::player::PlaybackState::Error,
            "the stop does not wait for the rollback"
        );
        assert!(!crate::route::flight_outstanding(), "the stop ends the rollback's flight");
        rig.flight.release_answers();

        rig.frames_until("the discarded replacement's stop", |r| replacement_stops(&r.flight) > stops);
        let now = engine_epoch(&mut rig);
        assert!(now.is_none() || now == epoch, "the rollback's landing reloads nothing");
        assert_eq!(crate::player::error_now(&rig.ps).kind, crate::player::FailureKind::ServerStopped);
    }

    #[test]
    fn a_stop_the_server_sent_for_another_playback_leaves_this_one_playing() {
        let mut rig = Rig::new(120);
        FEED_SHARED.frames.store(10, Relaxed);
        crate::player::note_server_stop("an-earlier-playback", "rig-1", "stopped by the admin", 10_000);
        rig.frame();
        rig.frame();
        assert_ne!(crate::player::state(&rig.ps), crate::player::PlaybackState::Error);
        assert!(rig.pa.is_live());
    }

    /// **A terminal playback error lets go of the pipeline.** The read-out stands, but the loaded
    /// pipeline and the video plane behind it made the television treat the app as still playing
    /// video: its screensaver timer was reset every 30 s for as long as the error was up. The Engine
    /// is released the frame after it fails, and the read-out keeps its cause and the resume point.
    #[test]
    fn a_terminal_error_releases_the_pipeline_and_keeps_the_read_out() {
        let mut rig = Rig::new(120);
        FEED_SHARED.frames.store(10, Relaxed);
        FEED_SHARED.seen_frame.store(true, Release);
        FEED_SHARED.demux_io_failed.store(true, Release);
        rig.pa.publish_feed_attempt_view(&mut rig.ps);
        let cause_before = crate::player::error_now(&rig.ps).kind;
        rig.frame();
        assert_eq!(crate::player::state(&rig.ps), crate::player::PlaybackState::Error);
        rig.frame();
        assert!(!rig.pa.is_live(), "the failed Engine and its pipeline are released");
        assert_eq!(crate::player::state(&rig.ps), crate::player::PlaybackState::Error, "the read-out stands without an Engine");
        assert_eq!(crate::player::error_now(&rig.ps).kind, cause_before, "and keeps its cause");
        assert_eq!(
            crate::route::unpresented_resume_ns(&rig.ps),
            60_000_000_000,
            "a retry resumes from where the playhead was"
        );
        rig.frame();
        assert_eq!(crate::player::state(&rig.ps), crate::player::PlaybackState::Error, "still, a frame later");
    }

    /// Back from the read-out after the release still ends the playback, once: the real stop runs
    /// with no Engine and the standing error is cleared with the session.
    #[test]
    fn leaving_the_read_out_after_the_release_stops_the_playback_once() {
        let mut rig = Rig::new(120);
        FEED_SHARED.frames.store(10, Relaxed);
        FEED_SHARED.demux_io_failed.store(true, Release);
        rig.frame();
        rig.frame();
        assert!(!rig.pa.is_live());
        crate::player::stop_bufferfeed(&mut rig.ps, &mut rig.pa);
        assert!(!rig.pa.is_live());
        crate::route::cancel_play(&mut rig.ps); // the exit ritual's other half
        assert_ne!(crate::player::state(&rig.ps), crate::player::PlaybackState::Error, "leaving clears the standing error");
        assert!(crate::route::url(&rig.ps).is_empty(), "and the playback");
        crate::player::stop_bufferfeed(&mut rig.ps, &mut rig.pa);
    }

    #[test]
    fn stopping_while_a_seek_reload_waits_for_unload_reports_the_target() {
        let mut rig = Rig::direct(0);
        rig.frame();
        suspend_bufferfeed(&mut rig.ps, &mut rig.pa);
        // Deliver the release owner's handoff for a previous object that still owns the slot.
        // Start, cancellation, and the Plex request below use the product paths.
        rig.pa.release_step(&mut rig.ps, &super::super::release::ReleaseEvent::UnloadHandedOff {
            epoch: 1, at: std::time::Instant::now(),
        }, &mut Vec::new());
        crate::player::request_seek(&mut rig.ps, &mut rig.pa, SEEK_TARGET_NS);
        assert_eq!(
            super::super::engine::reload_at(&mut rig.ps, &mut rig.pa, SEEK_TARGET_NS),
            super::super::engine::ReloadOutcome::Started,
        );
        assert!(rig.ps.readout().start_queued(), "the seek's replacement waits for Unload");
        assert!(!rig.pa.is_live(), "there is no engine to take a stop sample from");

        crate::player::stop_bufferfeed(&mut rig.ps, &mut rig.pa);
        crate::route::drain_scrobble();
        assert!(!rig.ps.readout().start_queued(), "Back cancels the replacement");
        let stopped: Vec<_> = rig.flight.requests().into_iter()
            .filter(|line| line.contains("/:/timeline?") && line.contains("state=stopped"))
            .collect();
        let target = format!("time={}&", SEEK_TARGET_NS / 1_000_000);
        assert_eq!(stopped.len(), 1, "one final Plex report: {stopped:?}");
        assert!(stopped[0].contains(&target), "the pending seek is the stop position: {stopped:?}");
    }

    /// Taps that arrive during a seek's flight coalesce to the LAST one: the first landing is
    /// outdated and discarded (its replacement stopped), and exactly one more flight runs, at the
    /// newest target.
    #[test]
    fn seek_taps_during_a_flight_coalesce_to_the_last() {
        let mut rig = Rig::new(120);
        crate::player::request_seek(&mut rig.ps, &mut rig.pa, SEEK_TARGET_NS);
        rig.frame();
        assert!(crate::route::flight_outstanding());
        // The worker is up and its answer is being held: the taps below arrive mid-flight.
        rig.flight.wait_for("the first rebuild's request", |f| f.decisions() == 1);
        crate::player::request_seek(&mut rig.ps, &mut rig.pa, SEEK_TARGET_NS + 30_000_000_000);
        crate::player::request_seek(&mut rig.ps, &mut rig.pa, SEEK_TARGET_NS + 60_000_000_000);
        rig.frame();
        assert_eq!(rig.flight.decisions(), 1, "the taps must not start a second rebuild while one flies");

        rig.frames_until("the newest target's landing", |r| {
            r.flight.decisions() == 2 && !crate::route::flight_outstanding()
        });
        let requests = rig.flight.requests();
        let decisions: Vec<&String> =
            requests.iter().filter(|r| r.contains("/decision?") && !r.contains("hasMDE=1")).collect();
        assert!(decisions[0].contains("offset=120.0"), "{}", decisions[0]);
        assert!(decisions[1].contains("offset=180.0"), "the last tap wins: {}", decisions[1]);
        rig.flight.wait_for("the outdated replacement's stop", |f| {
            f.stopped().iter().any(|s| s.starts_with("rig-logical-abr-"))
        });
        assert_eq!(FEED_TX.seek_to_ns.load(Relaxed), -1);
        assert_eq!(rig.ps.readout().pending_seek(), None);
    }

    // ---- a CLAIM flight that an app-switch suspend crosses ------------------------------------

    /// The session ids the server has been asked to register (`/decision`), in order.
    fn registered_sessions(flight: &FlightRig) -> Vec<String> {
        flight
            .requests()
            .iter()
            .filter(|r| r.contains("/decision?") && !r.contains("hasMDE=1"))
            .filter_map(|r| {
                let at = r.find("session=")? + "session=".len();
                Some(r[at..].split(['&', ' ']).next()?.to_owned())
            })
            .collect()
    }

    /// Dispatch whatever claim `queue` queues, let its worker reach the server, then do what the run
    /// loop does for Home and for the DID-foreground that follows: snapshot the intended position,
    /// `suspend_bufferfeed`, drain frames until the late landing is reaped, and drive the
    /// restore's own resume (`begin_resume` / `land_resume`). Grades the four things a claim that
    /// crosses a suspend must leave true: the flight and its hold are over, the restore is
    /// accepted and lands, and no encoder the worker registered runs on unowned.
    fn suspend_across_a_claim(mut rig: Rig, queue: impl FnOnce(&mut Rig), wait_for_request: bool) -> Rig {
        // This case suspends a watched stream, so publish its reported clock before claiming a
        // replacement. A seed position without a native report remains unknown in the product.
        FEED_SHARED.seen_frame.store(true, Release);
        rig.pa.publish_feed_attempt_view(&mut rig.ps);
        rig.frame();
        queue(&mut rig);
        rig.frame();
        assert!(crate::route::flight_outstanding(), "sanity: the claim must be flying");
        if wait_for_request {
            rig.flight.wait_for("the claim's request", |f| f.decisions() >= 1);
        }
        let saved_ns = crate::player::intended_pos_ns(&rig.ps);
        suspend_bufferfeed(&mut rig.ps, &mut rig.pa);

        assert!(!crate::route::flight_outstanding(), "the suspend left the claim's flight outstanding");
        assert!(!super::super::claim_hold::active(), "the suspend left the claim's hold up");
        assert!(crate::route::control_phase_label().contains("Stable"), "{}", crate::route::control_phase_label());

        // No Engine, no pump: the run loop's own per-frame reap (`land_resume` runs it first) is what
        // stops the late landing's replacement, and it must, with nothing outstanding to land.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while replacement_stops(&rig.flight) == 0 {
            {
                let _frame = plx_base::task::FrameScope::enter();
                assert!(land_resume(&mut rig.ps, &mut rig.pa).is_none(), "a dropped flight landed");
            }
            assert!(std::time::Instant::now() < deadline, "timed out waiting for the late landing's reap");
            std::thread::sleep(Duration::from_millis(5));
        }

        let started = {
            let _frame = plx_base::task::FrameScope::enter();
            begin_resume(&mut rig.ps, &mut rig.pa, saved_ns)
        };
        assert_eq!(started, ResumeStart::Pending, "the foreground restore's resume was refused");
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let _frame = plx_base::task::FrameScope::enter();
            if let Some(outcome) = land_resume(&mut rig.ps, &mut rig.pa) {
                assert_eq!(outcome, ResumeOutcome::Prepared);
                break;
            }
            assert!(std::time::Instant::now() < deadline, "timed out waiting for the restore's landing");
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(!crate::route::flight_outstanding());
        assert!(!super::super::claim_hold::active());

        // Every encoder a worker registered is either the route the restore installed or stopped.
        let route = crate::route::transcode_session(&rig.ps);
        rig.flight.wait_for("every unowned replacement's stop", |f| {
            let stopped = f.stopped();
            registered_sessions(f).iter().all(|id| *id == route || stopped.contains(id))
        });
        rig
    }

    /// A manual Original pick whose flight a suspend crosses: the restore is accepted, the hold is
    /// released, nothing leaks, and the viewer's pick is owed to the restored Engine.
    #[test]
    fn a_suspend_across_a_manual_original_claim_does_not_strand_the_restore() {
        let rig = Rig::new(120);
        let rig = suspend_across_a_claim(
            rig,
            |rig| {
                FlightRig::offer_original(&mut rig.ps, false);
                FlightRig::queue_manual_original(&rig.ps, true);
            },
            true,
        );
        assert!(
            crate::route::pending_user_route_intent(crate::route::UserRouteIntent::RecoverOriginal(
                crate::route::RecoveryCause::ManualOriginal
            )),
            "the pick the dropped claim owed is queued for the restored Engine"
        );
        drop(rig);
    }

    /// The Auto watchdog's HLS-to-Original handoff crossing a suspend: automatic, so nothing is owed
    /// (its ticket died with the Engine), but the restore must not be stranded.
    #[test]
    fn a_suspend_across_an_automatic_original_claim_does_not_strand_the_restore() {
        let rig = hls_rig(120);
        let rig = suspend_across_a_claim(
            rig,
            |rig| {
                FlightRig::offer_original(&mut rig.ps, false);
                FlightRig::publish_hls_to_original(60_000_000_000);
            },
            true,
        );
        assert!(!crate::route::pending_user_route_intent(crate::route::UserRouteIntent::Retranscode));
        drop(rig);
    }

    /// The Auto watchdog's Original-to-HLS fallback crossing a suspend.
    #[test]
    fn a_suspend_across_an_automatic_hls_fallback_does_not_strand_the_restore() {
        let rig = auto_original_rig(120);
        let rig = suspend_across_a_claim(
            rig,
            |_| FlightRig::publish_original_to_hls(6_000, 60_000_000_000),
            true,
        );
        drop(rig);
    }

    /// An adaptive refresh crossing a suspend.
    #[test]
    fn a_suspend_across_an_adaptive_reload_claim_does_not_strand_the_restore() {
        let rig = Rig::new(120);
        let rig = suspend_across_a_claim(rig, |rig| crate::player::request_adaptive_reload(&rig.ps), true);
        drop(rig);
    }

    // ---- the failure-path flights --------------------------------------------------------------

    /// A rig on the Original trial's failure edge: the viewer picked Original on a transcode, the
    /// recovery landed and reloaded onto the trial (the HLS `rig-1` is held as its way back), and
    /// the caller is about to let the source refuse to open.
    fn failed_original_trial_rig(delay_ms: u64) -> Rig {
        let mut rig = Rig::new(delay_ms);
        land_original_trial(&mut rig);
        rig
    }

    /// The viewer picks Original on `rig`'s transcode and the recovery lands: the Engine reloads
    /// onto the trial, and the HLS `rig-1` is held as its way back.
    fn land_original_trial(rig: &mut Rig) {
        FlightRig::offer_original(&mut rig.ps, false);
        rig.frame();
        FlightRig::queue_manual_original(&rig.ps, false);
        rig.frame();
        rig.frames_until("the Original trial's landing", |_| !crate::route::flight_outstanding());
        assert!(crate::route::original_recovery_pending(), "sanity: the trial is armed");
    }

    /// The stops the server has been asked for the encoders this rig's rebuilds register.
    fn replacement_stops(flight: &FlightRig) -> usize {
        flight.stopped().iter().filter(|s| s.starts_with("rig-logical-abr-")).count()
    }

    /// After the Original trial's source refused to open, the rollback rebases the restored HLS
    /// route at the recovery position on a worker: when the failure frame returns the server has
    /// been asked NOTHING, the viewer still sees the connecting spinner, and the rollback lands on
    /// a later frame.
    #[test]
    fn a_failed_original_trial_frame_makes_no_pms_call_and_the_worker_lands_the_rollback_later() {
        let mut rig = failed_original_trial_rig(150);
        let answered = rig.flight.answered();
        let decisions = rig.flight.decisions();
        let epoch = engine_epoch(&mut rig);

        rig.flight.hold_answers();
        FEED_SHARED.load_failed.store(true, Release);
        rig.frame();
        assert_eq!(
            rig.flight.answered(),
            answered,
            "the failure frame waited for a PMS answer on the frame thread: {:?}",
            rig.flight.requests()
        );
        assert!(crate::route::flight_outstanding(), "the rollback's rebase must be a flight");
        assert_ne!(crate::player::state(&rig.ps), crate::player::PlaybackState::Error, "the viewer sees a spinner, not the failure");
        assert_eq!(engine_epoch(&mut rig), epoch, "nothing reloads before the landing");
        rig.flight.release_answers();

        let landing_log = plx_base::eventlog::with_private_log(|| {
            rig.frames_until("the rollback's landing", |_| !crate::route::flight_outstanding());
            std::fs::read_to_string(plx_base::eventlog::events_log()).unwrap_or_default()
        });
        assert_eq!(rig.flight.decisions(), decisions + 1, "exactly one rebuild /decision for the rollback");
        assert!(!crate::route::original_recovery_pending(), "the way back was taken");
        assert_ne!(engine_epoch(&mut rig), epoch, "the landing reloaded onto the restored HLS");
        assert_ne!(crate::player::state(&rig.ps), crate::player::PlaybackState::Error,
            "{}\nlanding log:\n{}", rig.recovery_diag(), landing_log);
        // The rollback noted why the Original was refused (no HTTP status: the open failed after a
        // 200), and the reload that restored HLS kept it: the panel still explains the fallback.
        assert_eq!(rig.ps.readout().original_failure(), Some((crate::player::ABR_FAILURE_ORIGINAL_OPEN, 0)),
            "the rollback's reload must keep the cause it is explaining");
        rig.flight.wait_for("the restored HLS's retirement", |f| f.stopped().iter().any(|s| s == "rig-1"));
    }

    /// A refused rollback rebase ends in the failure read-out,
    /// the reducer `Failed`, nothing reloaded, the refused replacement stopped and the encoder the
    /// route still names left alone.
    #[test]
    fn a_refused_rollback_rebase_ends_in_the_error_state_and_stops_only_the_replacement() {
        let mut rig = failed_original_trial_rig(40);
        let epoch = engine_epoch(&mut rig);
        rig.flight.refuse_decisions(1);
        let answered = rig.flight.answered();

        rig.flight.hold_answers();
        FEED_SHARED.load_failed.store(true, Release);
        rig.frame();
        assert_eq!(rig.flight.answered(), answered, "{:?}", rig.flight.requests());
        assert!(crate::route::flight_outstanding());
        rig.flight.release_answers();

        rig.frames_until("the refusal's landing", |r| {
            crate::player::state(&r.ps) == crate::player::PlaybackState::Error
        });
        assert!(!crate::route::flight_outstanding());
        assert!(crate::route::control_phase_label().contains("Failed"), "{}", crate::route::control_phase_label());
        assert_eq!(engine_epoch(&mut rig), epoch, "a refused rollback reloads nothing");
        assert!(!crate::route::original_recovery_pending());
        assert!(!rig.flight.stopped().iter().any(|s| s == "rig-1"), "{:?}", rig.flight.stopped());
    }

    /// The failure flag stays up for every frame of the flight, and the failure branches re-enter
    /// each one: they must wait on the flight, not dispatch (or fail the Engine) again.
    #[test]
    fn a_rollback_in_flight_is_not_redispatched_or_failed_by_the_frames_that_wait_on_it() {
        let mut rig = failed_original_trial_rig(200);
        let decisions = rig.flight.decisions();
        FEED_SHARED.load_failed.store(true, Release);
        rig.frame();
        rig.flight.wait_for("the rollback's request", |f| f.decisions() == decisions + 1);
        for _ in 0..5 {
            rig.frame();
            assert!(crate::route::flight_outstanding(), "a waiting frame ended the flight");
            assert_ne!(crate::player::state(&rig.ps), crate::player::PlaybackState::Error);
        }
        assert_eq!(rig.flight.decisions(), decisions + 1, "a frame that waits must not dispatch again");
        rig.frames_until("the rollback's landing", |_| !crate::route::flight_outstanding());
        assert_eq!(rig.flight.decisions(), decisions + 1);
    }

    /// A teardown while the rollback flies discards its landing and stops only what the worker
    /// registered: nothing reloads and nothing is installed over the next playback.
    #[test]
    fn a_teardown_during_the_rollback_discards_the_landing_and_stops_only_its_replacement() {
        let mut rig = failed_original_trial_rig(150);
        let decisions = rig.flight.decisions();
        let epoch = engine_epoch(&mut rig);
        FEED_SHARED.load_failed.store(true, Release);
        rig.frame();
        rig.flight.wait_for("the rollback's request", |f| f.decisions() == decisions + 1);
        let stops = replacement_stops(&rig.flight);

        crate::route::begin_engine_teardown(false);
        // The worker is still waiting on the server: its landing arrives after the teardown, and
        // the next frame's drain finds it stale.
        rig.frames_until("the discarded replacement's stop", |r| replacement_stops(&r.flight) > stops);
        // The torn-down route leaves the Engine failed under the read-out, which releases it: it is
        // gone or it is the same one, never a reload onto the discarded replacement.
        let now = engine_epoch(&mut rig);
        assert!(now.is_none() || now == epoch, "a discarded landing reloads nothing");
        assert!(!rig.flight.stopped().iter().any(|s| s == "rig-1"), "{:?}", rig.flight.stopped());
    }

    // ---- the rollbacks no Engine waits on ------------------------------------------------------

    /// **A synchronous Load failure inside the Original trial** (`start_original_trial_reload`: the
    /// Engine could not even be constructed, so there is no Engine for a pump to recover) rebases
    /// the restored HLS route as an Engine-less flight: when the frame returns the server has been asked NOTHING, the viewer keeps a spinner (not the
    /// failure read-out), and the app loop's drain reloads onto the restored route when it lands.
    #[test]
    fn a_synchronous_trial_load_failure_makes_no_pms_call_and_the_worker_lands_the_rollback_later() {
        let mut rig = failed_original_trial_rig(150);
        let answered = rig.flight.answered();
        let decisions = rig.flight.decisions();
        let epoch = engine_epoch(&mut rig);
        // The candidate route is gone, so the trial's own reload cannot start.
        crate::route::clear_url(&mut rig.ps);

        rig.flight.hold_answers();
        {
            let _frame = plx_base::task::FrameScope::enter();
            start_original_trial_reload(&mut rig.ps, &mut rig.pa, crate::route::AutoOriginalReload::Remux, 60_000_000_000);
        }
        assert_eq!(
            rig.flight.answered(),
            answered,
            "the failure frame waited for a PMS answer on the frame thread: {:?}",
            rig.flight.requests()
        );
        assert!(crate::route::engineless_flight_outstanding(), "the rollback's rebase must be an Engine-less flight");
        assert_eq!(crate::player::state(&rig.ps), crate::player::PlaybackState::Resolving, "a spinner, not the failure");
        assert_eq!(engine_epoch(&mut rig), epoch, "nothing reloads before the landing");
        rig.flight.release_answers();

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while crate::route::engineless_flight_outstanding() {
            assert!(std::time::Instant::now() < deadline, "timed out waiting for the rollback's landing");
            std::thread::sleep(Duration::from_millis(2));
            let _frame = plx_base::task::FrameScope::enter();
            drain_engineless_rollback(&mut rig.ps, &mut rig.pa);
        }
        assert_eq!(rig.flight.decisions(), decisions + 1, "exactly one rebuild /decision for the rollback");
        assert!(!crate::route::original_recovery_pending(), "the way back was taken");
        assert_ne!(engine_epoch(&mut rig), epoch, "the landing reloaded onto the restored HLS");
        assert_ne!(crate::player::state(&rig.ps), crate::player::PlaybackState::Error,
            "{}", rig.recovery_diag());
        assert_eq!(rig.ps.readout().original_failure(), Some((crate::player::ABR_FAILURE_ORIGINAL_OPEN, 0)),
            "the Engine-less rollback noted the refusal, and the reload onto HLS kept it");
    }

    /// A refused rebase of that rollback ends in the failure read-out, nothing reloaded, the refused replacement stopped and the route's encoder alone.
    #[test]
    fn a_refused_engineless_rollback_ends_in_the_error_state() {
        let mut rig = failed_original_trial_rig(40);
        let epoch = engine_epoch(&mut rig);
        rig.flight.refuse_decisions(1);
        crate::route::clear_url(&mut rig.ps);
        start_original_trial_reload(&mut rig.ps, &mut rig.pa, crate::route::AutoOriginalReload::Remux, 60_000_000_000);
        assert!(crate::route::engineless_flight_outstanding());
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while crate::route::engineless_flight_outstanding() {
            assert!(std::time::Instant::now() < deadline, "timed out waiting for the refusal's landing");
            std::thread::sleep(Duration::from_millis(2));
            drain_engineless_rollback(&mut rig.ps, &mut rig.pa);
        }
        assert_eq!(crate::player::state(&rig.ps), crate::player::PlaybackState::Error);
        assert_eq!(engine_epoch(&mut rig), epoch, "a refused rollback reloads nothing");
        assert!(!rig.flight.stopped().iter().any(|s| s == "rig-1"), "{:?}", rig.flight.stopped());
    }

    /// **The foreground restore's rollback** (`recover_failed_foreground_original`): the same
    /// Engine-less flight. The frame answers `Pending` having asked the server nothing, and
    /// [`land_engineless_rollback`] answers `Waiting` until the worker lands, then reloads with a
    /// tracked attempt for the foreground machine to follow.
    #[test]
    fn a_foreground_rollback_is_pending_without_a_pms_call_and_lands_a_tracked_reload() {
        let mut rig = failed_original_trial_rig(120);
        let answered = rig.flight.answered();
        rig.flight.hold_answers();
        let recovery = {
            let _frame = plx_base::task::FrameScope::enter();
            recover_failed_foreground_original(&mut rig.ps)
        };
        assert!(matches!(recovery, ForegroundOriginalRecovery::Pending));
        assert_eq!(rig.flight.answered(), answered, "{:?}", rig.flight.requests());
        rig.flight.release_answers();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let landed = loop {
            assert!(std::time::Instant::now() < deadline, "timed out waiting for the rollback's landing");
            match land_engineless_rollback(&mut rig.ps, &mut rig.pa) {
                RollbackLanding::Waiting => std::thread::sleep(Duration::from_millis(2)),
                RollbackLanding::Stale => panic!("the flight went stale"),
                RollbackLanding::Landed(landed) => break landed,
            }
        };
        assert!(
            matches!(landed, ForegroundOriginalRecovery::Tracking(_)),
            "the landing reloads onto the restored HLS with an exact attempt for the foreground machine"
        );
        assert!(!crate::route::engineless_flight_outstanding());
    }

    /// A rig whose Original is an Auto watched source that never opened: nothing has been decoded.
    fn unopened_auto_original_rig(delay_ms: u64) -> Rig {
        let rig = {
            let mut rig = auto_original_rig(delay_ms);
            FlightRig::set_bootstrap_rung(&mut rig.ps, crate::abr::Rung::P720Low);
            rig
        };
        assert_eq!(FEED_SHARED.frames.load(Relaxed), 0, "sanity: no picture was ever decoded");
        rig
    }

    /// The unopened Auto Original's HLS fallback plans, registers the bootstrap rung's encoder
    /// (`/decision`) and installs it as the same flight as the watchdog's fallback, with nothing
    /// playing: when the failure frame returns the server has been asked NOTHING, the viewer sees
    /// the connecting spinner, and the HLS route lands on a later frame.
    #[test]
    fn an_unopened_source_frame_makes_no_pms_call_and_the_worker_lands_the_hls_later() {
        let mut rig = unopened_auto_original_rig(150);
        rig.frame();
        let answered = rig.flight.answered();
        let epoch = engine_epoch(&mut rig);

        rig.flight.hold_answers();
        FEED_SHARED.load_failed.store(true, Release);
        rig.frame();
        assert_eq!(
            rig.flight.answered(),
            answered,
            "the failure frame waited for a PMS answer on the frame thread: {:?}",
            rig.flight.requests()
        );
        assert!(crate::route::flight_outstanding(), "the unopened fallback's PMS half must be a flight");
        assert_ne!(crate::player::state(&rig.ps), crate::player::PlaybackState::Error, "a spinner, not the failure");
        assert_eq!(engine_epoch(&mut rig), epoch, "nothing reloads before the landing");
        rig.flight.release_answers();

        rig.frames_until("the fallback's landing", |_| !crate::route::flight_outstanding());
        assert_eq!(rig.flight.decisions(), 1, "exactly one replacement /decision");
        assert!(crate::route::is_segmented_hls(&rig.ps), "the route is HLS now");
        assert_ne!(engine_epoch(&mut rig), epoch, "the landing reloaded onto it at the failed position");
        assert_ne!(crate::player::state(&rig.ps), crate::player::PlaybackState::Error);
        rig.flight.wait_for("the replaced Original's stop", |f| f.stopped().iter().any(|s| s == "rig-1"));
    }

    /// A refused unopened fallback surfaces the failure the synchronous path did: the read-out,
    /// the reducer `Failed`, the route still the Original, nothing reloaded, the refused
    /// replacement stopped and the Original's encoder left to the teardown that owns it.
    #[test]
    fn a_refused_unopened_fallback_ends_in_the_error_state_and_leaves_the_route_alone() {
        let mut rig = unopened_auto_original_rig(40);
        rig.flight.refuse_decisions(1);
        rig.frame();
        let epoch = engine_epoch(&mut rig);
        let answered = rig.flight.answered();

        rig.flight.hold_answers();
        FEED_SHARED.load_failed.store(true, Release);
        rig.frame();
        assert_eq!(rig.flight.answered(), answered, "{:?}", rig.flight.requests());
        assert!(crate::route::flight_outstanding());
        rig.flight.release_answers();

        rig.frames_until("the refusal's landing", |r| {
            crate::player::state(&r.ps) == crate::player::PlaybackState::Error
        });
        assert!(!crate::route::flight_outstanding());
        assert!(crate::route::control_phase_label().contains("Failed"), "{}", crate::route::control_phase_label());
        assert_eq!(rig.flight.decisions(), 1);
        assert_eq!(crate::route::transcode_session(&rig.ps), "rig-1", "the Original is still the route");
        assert!(!crate::route::is_segmented_hls(&rig.ps));
        assert_eq!(engine_epoch(&mut rig), epoch, "a refusal reloads nothing");
        rig.flight.wait_for("the refused replacement's stop", |f| replacement_stops(f) == 1);
        assert!(!rig.flight.stopped().iter().any(|s| s == "rig-1"), "{:?}", rig.flight.stopped());
    }

    /// The same wait for the unopened fallback: the failure flag is up on every frame of the
    /// flight, and none of them may dispatch it again or fail the Engine out from under it.
    #[test]
    fn an_unopened_fallback_in_flight_is_not_redispatched_or_failed_by_the_frames_that_wait_on_it() {
        let mut rig = unopened_auto_original_rig(200);
        rig.frame();
        FEED_SHARED.load_failed.store(true, Release);
        rig.frame();
        rig.flight.wait_for("the fallback's request", |f| f.decisions() == 1);
        for _ in 0..5 {
            rig.frame();
            assert!(crate::route::flight_outstanding(), "a waiting frame ended the flight");
            assert_ne!(crate::player::state(&rig.ps), crate::player::PlaybackState::Error);
        }
        assert_eq!(rig.flight.decisions(), 1, "a frame that waits must not dispatch again");
        rig.frames_until("the fallback's landing", |_| !crate::route::flight_outstanding());
        assert_eq!(rig.flight.decisions(), 1);
    }

    /// This thread plays a set whose pipeline announces Opus, on a firmware the learned refusals key
    /// on (the process-wide answer is write-once; see `devcaps::firmware_on_this_thread_for_test`).
    fn firmware_announcing_opus() -> plx_platform::devcaps::CapsGuard {
        plx_platform::devcaps::firmware_on_this_thread_for_test(
            "kisscurl-koli",
            "6.5.3-47",
            plx_platform::devcaps::Caps {
                audio: "aac,ac3,eac3,opus".into(),
                ..plx_platform::devcaps::Caps::assumed()
            },
        )
    }

    /// **The pipeline's refusal of a natively fed audio track, before any picture, is a recovery.**
    /// `type=18 num=200` while the demuxer feeds Opus natively is learned for this firmware, and the
    /// play continues on the same recovery flight as an Original that never opened: the frame asks the
    /// server nothing, the viewer keeps the connecting spinner, the replacement route (the server
    /// converting the audio the TV just refused) lands on a later frame and the Engine reloads onto it
    /// at the failed position. Not the failure read-out, and not a second play request.
    #[test]
    fn a_native_audio_refusal_before_the_first_frame_ends_in_a_recovery_flight_not_error() {
        let mut rig = Rig::new(150);
        let _set = firmware_announcing_opus();
        let _session = plx_plex::plex::session::TempSession::new("named-opus-flight");
        rig.flight.begin_attempt(&rig.ps);
        // The route was planned before the refusal, from the capability value read then: Opus at
        // any channel count. The attempt holds that value, not the process-wide answer.
        FlightRig::plan_on(&mut rig.ps, plx_platform::devcaps::caps());
        FlightRig::declare_audio(&mut rig.ps, "opus", 6);
        rig.frame();
        let answered = rig.flight.answered();
        let epoch = engine_epoch(&mut rig);
        let opus_channels = || plx_platform::devcaps::caps().audio_channels.get("opus").copied();
        assert_eq!(opus_channels(), None, "the set announces Opus at any channel count until it is refused");

        plx_play_feed::native_audio::publish_fed(plx_play_feed::native_audio::Kind::Opus, 6);
        rig.flight.hold_answers();
        FEED_SHARED.raise_native_audio_refused();
        FEED_SHARED.load_failed.store(true, Release);
        rig.frame();
        assert_eq!(
            opus_channels(),
            None,
            "the named write is queued, and the process-wide answer is write-once: it never moves"
        );
        assert_ne!(
            crate::player::state(&rig.ps),
            crate::player::PlaybackState::Error,
            "a refused native audio track must not end on the failure read-out"
        );
        assert!(crate::route::flight_outstanding(), "the conversion's PMS half must be a recovery flight");
        assert_eq!(
            rig.flight.answered(),
            answered,
            "the failure frame waited for a PMS answer on the frame thread: {:?}",
            rig.flight.requests()
        );
        assert_eq!(engine_epoch(&mut rig), epoch, "nothing reloads before the landing");
        rig.flight.release_answers();

        rig.frames_until("the conversion's landing", |_| !crate::route::flight_outstanding());
        assert_eq!(rig.flight.decisions(), 1, "exactly one replacement /decision");
        // **The refusal learned mid-attempt reaches the recovery flight.** The attempt started on a
        // value that lists Opus without a ceiling; the dispatch narrowed that value by the refusal
        // (`dispatch_refusal_conversion`), so the replacement request's profile carries the
        // 2-channel ceiling the install just learned.
        let decisions = rig.flight.decision_requests();
        let replacement = decisions.first().expect("the replacement /decision");
        assert!(
            replacement.contains("scopeName%3Dopus%26type%3DupperBound%26name%3Daudio.channels%26value%3D2"),
            "the replacement profile must carry the learned Opus ceiling: {replacement}"
        );
        assert_ne!(engine_epoch(&mut rig), epoch, "the landing reloaded onto the converted route");
        assert_ne!(crate::player::state(&rig.ps), crate::player::PlaybackState::Error);
        let context = super::super::report::error_context_for_test(&rig.ps);
        assert_eq!(
            context.attempt_origin,
            super::super::report::AttemptOrigin::Recovery,
            "the converted open is the app's own recovery, so a failure of it must not read as the first fresh open"
        );
        assert_eq!(context.attempt_ordinal, super::super::report::AttemptOrdinal::FIRST.next());
        // The earlier `None` was not vacuous: the named row was written, and the NEXT play's start
        // value (`devcaps::start_caps`) carries the ceiling the record taught. The boot answer
        // stays as it was.
        plx_base::storage_worker::drain_for_test();
        plx_plex::plex::session::save(&plx_plex::plex::session::peek());
        assert_eq!(opus_channels(), None, "the landed record moved the process-wide answer");
        let start = plx_platform::devcaps::start_caps(&plx_plex::plex::session::peek().applied_refusals());
        assert_eq!(start.audio_channels.get("opus").copied(), Some(2), "the landed record gives the next start the ceiling");
        rig.flight.wait_for("the replaced encoder's stop", |f| f.stopped().iter().any(|s| s == "rig-1"));
    }

    /// **A refusal learned during the Original trial narrows the attempt too, though the rollback
    /// wins.** The trial's failed open takes the rollback (`OpenRecovery`'s ladder puts it before the
    /// conversion), and the refusal was learned all the same: the restored HLS route's rebase, and
    /// every retry and quality change after it, must plan and profile from the attempt's narrowed
    /// value, not the one the play started on. Else they offer native Opus again and the pipeline
    /// refuses it again: the request has spent a flight on a refusal its own rollback should
    /// already have converted.
    #[test]
    fn a_native_audio_refusal_learned_during_the_original_trial_reaches_the_rollbacks_profile() {
        let mut rig = Rig::new(150);
        let _set = firmware_announcing_opus();
        // The play began on the value the set announced: Opus at any channel count.
        FlightRig::plan_on(&mut rig.ps, plx_platform::devcaps::caps());
        FlightRig::declare_audio(&mut rig.ps, "opus", 6);
        land_original_trial(&mut rig);
        let decisions = rig.flight.decisions();
        let epoch = engine_epoch(&mut rig);

        plx_play_feed::native_audio::publish_fed(plx_play_feed::native_audio::Kind::Opus, 6);
        rig.flight.hold_answers();
        FEED_SHARED.raise_native_audio_refused();
        FEED_SHARED.load_failed.store(true, Release);
        rig.frame();
        assert!(crate::route::flight_outstanding(), "the rollback's rebase must be a flight");
        assert_ne!(crate::player::state(&rig.ps), crate::player::PlaybackState::Error);
        rig.flight.release_answers();

        rig.frames_until("the rollback's landing", |_| !crate::route::flight_outstanding());
        assert_eq!(rig.flight.decisions(), decisions + 1, "exactly one rebuild /decision for the rollback");
        assert!(!crate::route::original_recovery_pending(), "the rollback won, as it does");
        assert_ne!(engine_epoch(&mut rig), epoch, "the landing reloaded onto the restored HLS");
        let decisions = rig.flight.decision_requests();
        let rebase = decisions.last().expect("the rollback's /decision");
        assert!(
            rebase.contains("scopeName%3Dopus%26type%3DupperBound%26name%3Daudio.channels%26value%3D2"),
            "the rollback's profile must carry the Opus ceiling the pipeline just refused at: {rebase}"
        );
        rig.flight.wait_for("the restored HLS's retirement", |f| f.stopped().iter().any(|s| s == "rig-1"));
    }

    /// A conversion the server refuses leaves the failure the pump held back: the read-out, the
    /// route still the one the pipeline refused, nothing reloaded, the refused replacement stopped.
    /// The refusal stays learned, so a later play is converted from its first request.
    #[test]
    fn a_refused_native_audio_conversion_ends_in_the_error_state_and_leaves_the_route_alone() {
        let mut rig = Rig::new(40);
        let _set = firmware_announcing_opus();
        // The play began on the value the set announced (Opus at any width), as in the tests above:
        // an attempt which never announced Opus has nothing to narrow, so it gets no conversion.
        FlightRig::plan_on(&mut rig.ps, plx_platform::devcaps::caps());
        FlightRig::declare_audio(&mut rig.ps, "opus", 6);
        rig.flight.refuse_decisions(1);
        rig.frame();
        let epoch = engine_epoch(&mut rig);

        plx_play_feed::native_audio::publish_fed(plx_play_feed::native_audio::Kind::Opus, 6);
        FEED_SHARED.raise_native_audio_refused();
        FEED_SHARED.load_failed.store(true, Release);
        rig.frame();
        assert!(crate::route::flight_outstanding(), "the conversion must be a flight, not an immediate failure");

        rig.frames_until("the refusal's landing", |r| crate::player::state(&r.ps) == crate::player::PlaybackState::Error);
        assert!(!crate::route::flight_outstanding());
        assert_eq!(rig.flight.decisions(), 1, "one conversion request, never a loop");
        assert_eq!(crate::route::transcode_session(&rig.ps), "rig-1", "the refused route is still the route");
        assert_eq!(engine_epoch(&mut rig), epoch, "a refusal reloads nothing");
        rig.flight.wait_for("the refused replacement's stop", |f| replacement_stops(f) == 1);
        assert!(!rig.flight.stopped().iter().any(|s| s == "rig-1"), "{:?}", rig.flight.stopped());
    }

    /// **A native audio STALL of a combination the install already knew ends on the read-out, never
    /// on the buffering spinner.** TV run G (2026-10-26): a stored `{opus, 6ch}` refusal, and a
    /// transcode fixture serving 1600 B 6-channel Opus again. The watchdog fired and the learner
    /// said "known", which the pump took for "nothing to do": no Engine failure, no read-out, the
    /// watchdog spent, and the viewer on "Buffering..." for good. A known combination fails like any
    /// refusal the ladder has no rung for: the Engine fails and the read-out shows.
    #[test]
    fn a_native_audio_stall_of_a_known_combination_ends_on_the_read_out_not_the_spinner() {
        use plx_play_feed::native_stall::{Watch, NATIVE_AUDIO_STALL_MS, WATCH};
        let reset_watch = || *WATCH.lock().unwrap_or_else(|e| e.into_inner()) = Watch::new();
        let mut rig = Rig::new(40);
        let _set = firmware_announcing_opus();
        reset_watch();
        // The install learned this combination on an earlier play: the stored record holds it named,
        // and the play start derives its value from the record, so the attempt already lacks it.
        let _session = plx_plex::plex::session::TempSession::new("known-stall");
        let known = plx_plex::plex::session::NativeAudioRefusal::of_facet(
            &plx_platform::devcaps::Facet::Audio { codec: "opus".into(), channels: 6 },
            "kisscurl-koli",
            "6.5.3-47",
            None,
        )
        .expect("a named row");
        plx_plex::plex::session::save(&plx_plex::plex::session::peek().with_native_audio_refusal(&known).expect("news"));
        FlightRig::plan_on(&mut rig.ps, plx_platform::devcaps::start_caps(&plx_plex::plex::session::peek().applied_refusals()));
        assert!(!crate::route::audio_track_direct_plays(&rig.ps, "opus", 6, 48_000), "sanity: the attempt lacks the stored refusal");
        plx_play_feed::native_audio::publish_fed(plx_play_feed::native_audio::Kind::Opus, 6);
        let epoch = engine_epoch(&mut rig).expect("the rig's Engine");
        assert!(FEED_SHARED.mark_native_load_returned(epoch, None).is_some(), "the Load returned: the watchdog's precondition");
        // The pipeline takes one AU per lane, then presents nothing. Keep later AUs beyond the
        // feed-ahead limit and one more queued behind each pending AU, as a demuxer sees when a
        // decoder swallows the feed. The first frame completes Load and accepts the first pair;
        // the second establishes the watchdog's unchanged accepted-count baseline. The audio
        // lane DROPS an AU more than `AUDIO_STALE_AHEAD_NS` (5 s) past the fed video instead of
        // holding it, so its later AUs sit under that bound yet past its own feed-ahead budget
        // (`MAX_FEED_AHEAD_NS` + `AUDIO_SLACK_NS`, 3.6 s).
        {
            let mut guard = rig.pa.engine().expect("the rig's Engine");
            let eng = &mut *guard;
            eng.stage = Stage::Loading;
            for (queue, es, later) in [
                (eng.aq_video.as_mut(), 1, [6_000_000_000, 6_100_000_000]),
                (eng.aq_audio.as_mut(), 0, [5_000_000_000, 5_100_000_000]),
            ] {
                let q = &mut **queue.expect("the Engine's AU queues") as *mut plx_play_feed::aq::AuQueue;
                for pts in [1_000_000_000, later[0], later[1]] {
                    assert_eq!(plx_play_feed::aq::aq_push(q, [0u8; 64].as_ptr(), 64, pts, 1, es), 0);
                }
            }
        }
        let log = plx_base::eventlog::with_private_log(|| {
            rig.frame();
            assert_ne!(crate::player::state(&rig.ps), crate::player::PlaybackState::Error, "the watchdog only armed");
            rig.frame();
            assert_ne!(crate::player::state(&rig.ps), crate::player::PlaybackState::Error, "accepted progress is not a stall");
            rig.now += NATIVE_AUDIO_STALL_MS + 100;
            rig.frame();
            std::fs::read_to_string(plx_base::eventlog::events_log()).unwrap_or_default()
        });
        let state = crate::player::state(&rig.ps);
        reset_watch();
        assert!(log.contains("native audio: STALL, no picture"), "PRECONDITION FAILED, not the defect: the watchdog did not fire:\n{log}");
        assert_eq!(
            state,
            crate::player::PlaybackState::Error,
            "a stall of a combination the install already knew left the viewer on {state:?}"
        );
    }

    // ---- S1a: a pipeline refusal of a stream the attempt can do without --------------------------

    /// The set the attempt started on: its pipeline announces DTS at six channels (the shape of
    /// `plan_track_selection_tests`' DTS row), so taking `dts:6` out of the attempt is a real
    /// narrowing and the facet rung has something to convert.
    fn dts_announcing_start() -> std::sync::Arc<plx_platform::devcaps::Caps> {
        std::sync::Arc::new(plx_platform::devcaps::Caps {
            audio: "aac,ac3,eac3,dts".into(),
            audio_channels: [("dts".into(), 6)].into(),
            ..plx_platform::devcaps::Caps::assumed()
        })
    }

    /// `rig` plays a DTS 5.1 track which the attempt (begun on [`dts_announcing_start`]) feeds to
    /// the pipeline, as the plan of a direct play of that track would have declared it.
    fn declare_dts(rig: &mut Rig) {
        FlightRig::plan_on(&mut rig.ps, dts_announcing_start());
        FlightRig::declare_audio(&mut rig.ps, "dca", 6);
    }

    /// [`declare_dts`] on a rig whose playback has a report attempt (a Play's generation).
    fn dts_rig(rig: Rig) -> Rig {
        let mut rig = rig;
        rig.flight.begin_attempt(&rig.ps);
        declare_dts(&mut rig);
        rig
    }

    /// The pipeline's `type=18 num=601` (Resource Allocation Error) after the Load returned, through
    /// the callback the firmware calls: the same entry `inject_refuse601` publishes on.
    fn pipeline_refuses_601(rig: &mut Rig) {
        let epoch = engine_epoch(rig).expect("the rig's Engine");
        plx_play_feed::sf_event::sf_on_event(epoch, 18, 601, std::ptr::null());
    }

    /// **The pipeline takes the exact Load a test just started.** A host Load can now either
    /// succeed after a proved release or refuse when the initial synthetic Engine left an object.
    /// Wait for the current epoch's worker result, then retire only a synchronous host refusal:
    /// the next failure these tests grade is their own callback (`pipeline_refuses_601`).
    fn pipeline_takes_the_load(rig: &mut Rig) {
        let epoch = engine_epoch(rig).expect("the started Load's Engine");
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !FEED_SHARED.native_load_returned(epoch) && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(FEED_SHARED.native_load_returned(epoch), "the exact native Load never returned");
        FEED_SHARED.load_failed.store(false, Release);
    }

    /// What a refusal-recovery landing for a DTS 5.1 track leaves, after the flight landed on an
    /// Engine that was at `epoch`: one replacement `/decision` converting the DTS track (the
    /// profile carries the stereo bound, the request names the track the viewer was on, so no
    /// sibling is chosen in S1a), the Engine reloaded onto it, the open counted once as the app's
    /// own recovery, and the replaced encoder stopped.
    fn assert_dts_converted_in_one_flight(rig: &mut Rig, epoch: Option<u32>) {
        assert_eq!(rig.flight.decisions(), 1, "exactly one replacement /decision");
        let decisions = rig.flight.decision_requests();
        let replacement = decisions.first().expect("the replacement /decision");
        assert!(
            replacement.contains("scopeName%3Ddca%26type%3DupperBound%26name%3Daudio.channels%26value%3D2"),
            "the replacement profile must bound DTS to two channels: {replacement}"
        );
        assert!(
            replacement.contains("audioStreamID=960002"),
            "the replacement converts the DTS track the viewer was on: {replacement}"
        );
        assert_ne!(engine_epoch(rig), epoch, "the landing reloaded onto the converted route");
        assert_ne!(crate::player::state(&rig.ps), crate::player::PlaybackState::Error,
            "{}", rig.recovery_diag());
        let context = super::super::report::error_context_for_test(&rig.ps);
        assert_eq!(context.attempt_origin, super::super::report::AttemptOrigin::Recovery);
        assert_eq!(context.attempt_ordinal, super::super::report::AttemptOrdinal::FIRST.next(), "counted once");
        rig.flight.wait_for("the replaced encoder's stop", |f| f.stopped().iter().any(|s| s == "rig-1"));
    }

    /// Drive a DTS 601 on `rig` to the converted route's landing and grade it
    /// ([`assert_dts_converted_in_one_flight`]).
    fn dts_601_converts_in_one_flight(rig: &mut Rig) {
        rig.frame();
        let answered = rig.flight.answered();
        let epoch = engine_epoch(rig);
        rig.flight.hold_answers();
        pipeline_refuses_601(rig);
        rig.frame();
        assert_ne!(
            crate::player::state(&rig.ps),
            crate::player::PlaybackState::Error,
            "a refused DTS track must not end on the failure read-out"
        );
        assert!(crate::route::flight_outstanding(), "the conversion's PMS half must be a recovery flight");
        assert_eq!(
            rig.flight.answered(),
            answered,
            "the failure frame waited for a PMS answer on the frame thread: {:?}",
            rig.flight.requests()
        );
        assert_eq!(engine_epoch(rig), epoch, "nothing reloads before the landing");
        rig.flight.release_answers();
        rig.frames_until("the conversion's landing", |_| !crate::route::flight_outstanding());
        assert_dts_converted_in_one_flight(rig, epoch);
    }

    /// **#695.** A DTS 5.1 direct play that the pipeline refuses with `type=18 num=601` before the
    /// first picture used to end on the failure read-out (`Error`): the refusal named no native
    /// audio, so the ladder had no rung. It now converts the suspect stream: the frame asks the
    /// server nothing, the viewer keeps the connecting spinner, ONE recovery flight asks for the
    /// DTS track converted under a profile that bounds DTS to stereo, and the Engine reloads onto it.
    ///
    /// **Recorded deviation from the plan (s1a-design section 1):** the conversion landing keeps the
    /// current track (`audioStreamID=960002` is the DTS track the viewer was on), and no AC3 sibling
    /// is picked in S1a, whether or not the Part has one. The sibling falls out of the planner on the
    /// first request after the SECOND rescued play, once the counted row applies and sits in `start`.
    /// So a Part whose only audio is the DTS track takes this one path too.
    #[test]
    fn a_dts_601_before_the_first_picture_converts_dts_in_one_flight() {
        dts_601_converts_in_one_flight(&mut dts_rig(Rig::new(150)));
    }

    /// `rig` plays a 4K H.264 with AC-3 5.1 audio (a codec every set decodes, so the audio is never
    /// the suspect), as the plan of a direct play of it would have declared it.
    fn declare_4k_h264(rig: &mut Rig) {
        rig.flight.begin_attempt(&rig.ps);
        FlightRig::declare_video(&mut rig.ps, "h264", 3840, 2160);
        FlightRig::declare_audio(&mut rig.ps, "ac3", 6);
    }

    /// Drive a `type=18 num=100` on `rig` to its recovery flight's landing and grade what any such
    /// flight owes: the failure frame is not the read-out and waits on no PMS answer, ONE flight
    /// carries the replacement `/decision`, nothing reloads before it lands, and the landing reloads
    /// the Engine onto it. Returns the replacement's request line.
    fn refuse_100_and_land_one_flight(rig: &mut Rig) -> String {
        refuse_and_land_flight(rig, 100, 1)
    }

    /// [`refuse_100_and_land_one_flight`] for the request's `n`th flight: `n` replacement
    /// `/decision`s have been asked once it lands.
    fn refuse_100_and_land_next_flight(rig: &mut Rig, n: usize) -> String {
        refuse_and_land_flight(rig, 100, n)
    }

    /// The same with the DTS refusal's number.
    fn refuse_601_and_land_flight(rig: &mut Rig, n: usize) -> String {
        refuse_and_land_flight(rig, 601, n)
    }

    fn refuse_and_land_flight(rig: &mut Rig, num: i64, n: usize) -> String {
        rig.frame();
        let answered = rig.flight.answered();
        let epoch = engine_epoch(rig);
        rig.flight.hold_answers();
        let refusal_log = plx_base::eventlog::with_private_log(|| {
            plx_play_feed::sf_event::sf_on_event(epoch.expect("the rig's Engine"), 18, num, std::ptr::null());
            rig.frame();
            std::fs::read_to_string(plx_base::eventlog::events_log()).unwrap_or_default()
        });
        assert_ne!(
            crate::player::state(&rig.ps),
            crate::player::PlaybackState::Error,
            "a refused stream must not end on the failure read-out (flight {n}): {}\nrefusal log:\n{}",
            rig.recovery_diag(), refusal_log
        );
        assert!(crate::route::flight_outstanding(), "the conversion's PMS half must be a recovery flight");
        assert_eq!(rig.flight.answered(), answered, "the failure frame waited for a PMS answer: {:?}", rig.flight.requests());
        assert_eq!(engine_epoch(rig), epoch, "nothing reloads before the landing");
        rig.flight.release_answers();
        rig.frames_until("the conversion's landing", |_| !crate::route::flight_outstanding());
        assert_eq!(rig.flight.decisions(), n, "exactly {n} replacement /decisions");
        let replacement = rig.flight.decision_requests().last().cloned().expect("the replacement /decision");
        assert_ne!(engine_epoch(rig), epoch, "the landing reloaded onto the converted route");
        assert_ne!(crate::player::state(&rig.ps), crate::player::PlaybackState::Error);
        replacement
    }

    /// **A 4K H.264 the pipeline refuses (`type=18 num=100`) before the first picture**, in a manual
    /// pick, used to end on the failure read-out. The video facet rung now flies ONE recovery flight
    /// for it: the replacement is an ENCODE (`directStream=0`, the audio still copied) under a profile
    /// that bounds H.264 to the 1080p floor and a `videoResolution` of 1080p, the carried audio track
    /// is the one the viewer was on, and the Engine reloads onto it. The converted route declares a
    /// sink envelope no larger than 1080p, not the 4K60 of the file it came from.
    #[test]
    fn a_4k_h264_refused_with_100_in_a_manual_pick_lands_a_1080p_conversion_with_the_audio_copied() {
        let mut rig = Rig::new(150);
        declare_4k_h264(&mut rig);
        let replacement = refuse_100_and_land_one_flight(&mut rig);
        for needle in [
            "scopeName%3Dh264%26type%3DupperBound%26name%3Dvideo.height%26value%3D1088",
            "directStream=0",
            "directStreamAudio=1",
            "audioStreamID=960002",
            "videoResolution=1920x1080",
        ] {
            assert!(replacement.contains(needle), "the replacement lacks {needle}: {replacement}");
        }
        let context = super::super::report::error_context_for_test(&rig.ps);
        assert_eq!(context.attempt_origin, super::super::report::AttemptOrigin::Recovery);
        // The ceiling the encode flew under bounds the sink envelope: the 1080p rung's raster, below
        // the profile floor's 1088.
        assert_eq!(crate::route::sink_max_raster(&rig.ps), (1920, 1080));
    }

    /// **A video refusal whose audio is the suspect converts the audio first.** A 4K H.264 behind a
    /// DTS 5.1 track that the pipeline refuses with `num=100`: the audio is outside the codecs every
    /// set decodes, so it is the suspect and the video is not considered. The first flight keeps the
    /// route as planned (`directStream=1`, the video still copied) under a profile that bounds DTS to
    /// stereo and names no H.264 bound. The route is an Original remux (a stored remux candidate), so
    /// it still copies the 4K video and the converted route's refusal takes the manual both-facets
    /// rung: a second flight with the H.264 bound too and `directStream=0` (the audio still copied),
    /// under the 1080p rung's ceiling.
    #[test]
    fn a_video_refusal_whose_audio_is_the_suspect_converts_the_audio_first() {
        let mut rig = dts_rig(Rig::new(150));
        FlightRig::declare_video(&mut rig.ps, "h264", 3840, 2160);
        FlightRig::offer_original_of_video(&mut rig.ps, false, "h264");
        let replacement = refuse_100_and_land_one_flight(&mut rig);
        for needle in [
            "scopeName%3Ddca%26type%3DupperBound%26name%3Daudio.channels%26value%3D2",
            "directStream=1",
            "audioStreamID=960002",
        ] {
            assert!(replacement.contains(needle), "the replacement lacks {needle}: {replacement}");
        }
        assert!(!replacement.contains("scopeName%3Dh264"), "the video is not the suspect, so it is not bounded: {replacement}");

        pipeline_takes_the_load(&mut rig);
        let second = refuse_100_and_land_next_flight(&mut rig, 2);
        for needle in [
            "scopeName%3Ddca%26type%3DupperBound%26name%3Daudio.channels%26value%3D2",
            "scopeName%3Dh264%26type%3DupperBound%26name%3Dvideo.height%26value%3D1088",
            "directStream=0",
            "directStreamAudio=1",
            "videoResolution=1920x1080",
        ] {
            assert!(second.contains(needle), "the both-facets replacement lacks {needle}: {second}");
        }
        assert_eq!(super::super::recovery::MAX_RECOVERY_FLIGHTS, 2, "the both rung spends the request's last flight");
    }

    /// **The manual both-facets rung.** DTS 5.1 behind a 1080p H.264: the DTS refusal (`601`) converts
    /// the audio, the converted remux is refused again (the frame rate, say: nothing of the raster
    /// can be lowered), and the second flight is an ENCODE of the video (`directStream=0`, the
    /// audio still copied) planned from the attempt, which keeps the route's own ceiling and names no
    /// H.264 bound. A third refusal reads out: the request's flights are spent and no third
    /// `/decision` is asked.
    #[test]
    fn the_manual_both_facets_rung_converts_both() {
        let mut rig = dts_rig(Rig::new(150));
        FlightRig::declare_video(&mut rig.ps, "h264", 1920, 1080);
        FlightRig::offer_original_of_video(&mut rig.ps, false, "h264");
        refuse_601_and_land_flight(&mut rig, 1);
        pipeline_takes_the_load(&mut rig);
        let second = refuse_601_and_land_flight(&mut rig, 2);
        for needle in [
            "scopeName%3Ddca%26type%3DupperBound%26name%3Daudio.channels%26value%3D2",
            "directStream=0",
            "directStreamAudio=1",
            "audioStreamID=960002",
        ] {
            assert!(second.contains(needle), "the both-facets replacement lacks {needle}: {second}");
        }
        assert!(!second.contains("scopeName%3Dh264"), "a 1080p raster is not lowered: {second}");
        assert!(second.contains("videoResolution=3840x2160"), "nothing was lowered, so the route keeps its own ceiling: {second}");

        pipeline_takes_the_load(&mut rig);
        let reloaded = engine_epoch(&mut rig).expect("the encoded route's Engine");
        rig.frame();
        plx_play_feed::sf_event::sf_on_event(reloaded, 18, 601, std::ptr::null());
        rig.frame();
        assert_eq!(crate::player::state(&rig.ps), crate::player::PlaybackState::Error, "the third refusal reads out");
        assert!(!crate::route::flight_outstanding(), "no third flight");
        assert_eq!(rig.ps.open_recovery().phase(), super::super::recovery::Phase::Failed);
        for _ in 0..3 {
            rig.frame();
        }
        assert_eq!(rig.flight.decisions(), 2, "the request's two flights are its only /decisions");
    }

    /// **The climb after a video rescue never re-offers the refused raster.** A manual Original
    /// pick keeps its stored candidate (the way back from an enhanced remux) and the candidate here
    /// carries the 4K H.264 the pipeline refused. A remux candidate COPIES the video too, so no
    /// candidate can avoid the source's raster: the landing drops it, and the menu's reason reads
    /// "video needs converting".
    #[test]
    fn the_climb_after_a_video_rescue_never_reoffers_the_refused_raster() {
        let mut rig = Rig::new(150);
        declare_4k_h264(&mut rig);
        FlightRig::offer_original_of_video(&mut rig.ps, true, "h264");
        assert_eq!(FlightRig::original_feeds_part(&rig.ps), Some(true), "sanity: the candidate would feed the 4K Part");
        assert_eq!(FlightRig::original_declined(&rig.ps), None, "sanity: nothing declined the Original yet");
        refuse_100_and_land_one_flight(&mut rig);
        assert_eq!(FlightRig::original_feeds_part(&rig.ps), None, "the refused raster is not offered again");
        assert_eq!(
            FlightRig::original_declined(&rig.ps),
            Some(crate::route::OriginalDeclined::VideoNeedsConverting),
            "the candidate was dropped for its video"
        );
    }

    /// **A cold Auto Original keeps its HLS contingency as the video re-encode, with the refused
    /// raster out of it.** The HLS rung is already an Auto Original's ABR-managed video re-encode, so a
    /// refused video does not fly a progressive encode first. Here the source never opened and its
    /// bootstrap rung is the 4K one: the unopened-source fallback re-checks that rung against the
    /// catalog of the NARROWED attempt (`original_open_fallback_rung`), where a 4K H.264 is
    /// infeasible, so the replacement flies the best rung under it, a 1080p one, instead of asking
    /// for `3840x2160` again. The route's sink envelope is no wider than 1080p, and the stored
    /// Original candidate (a 4K H.264) is dropped rather than climbed back to.
    #[test]
    fn the_hls_rescue_of_a_4k_h264_original_leaves_the_refused_raster_and_never_climbs_back() {
        let mut rig = auto_original_rig(60);
        declare_4k_h264(&mut rig);
        FlightRig::set_bootstrap_rung(&mut rig.ps, crate::abr::Rung::Uhd);
        FlightRig::offer_original_of_video(&mut rig.ps, true, "h264");
        assert_eq!(crate::route::sink_max_raster(&rig.ps), (3840, 2160), "sanity: the Original declares the 4K source");
        assert_eq!(FlightRig::original_feeds_part(&rig.ps), Some(true), "sanity: the candidate would feed the 4K Part");
        let replacement = refuse_100_and_land_one_flight(&mut rig);
        assert!(crate::route::is_segmented_hls(&rig.ps), "a cold Auto Original takes the HLS contingency");
        let asked = replacement
            .split("videoResolution=")
            .nth(1)
            .and_then(|rest| rest.split('&').next())
            .and_then(|raster| raster.split_once('x'))
            .and_then(|(w, h)| Some((w.parse::<u32>().ok()?, h.parse::<u32>().ok()?)))
            .unwrap_or_else(|| panic!("the replacement names no videoResolution: {replacement}"));
        assert_eq!(asked, (1920, 1080), "the best rung under the refused 4K one: {replacement}");
        let (width, height) = crate::route::sink_max_raster(&rig.ps);
        assert!(width <= 1920 && height <= 1080, "the HLS route declares {width}x{height}");
        assert_eq!(FlightRig::original_feeds_part(&rig.ps), None, "the refused raster is not offered again");
        assert_eq!(
            FlightRig::original_declined(&rig.ps),
            Some(crate::route::OriginalDeclined::VideoNeedsConverting),
            "the candidate was dropped for its video"
        );
    }

    /// A cold Auto Original refused on its DTS flies the conversion, not the HLS contingency: the
    /// facet rung comes before the both-facets rung, so the Original's remux is kept (the stream
    /// keeps its video bytes), no Original failure is noted, and the route is not segmented HLS.
    #[test]
    fn an_auto_original_refused_on_dts_flies_the_conversion_not_hls() {
        let mut rig = dts_rig(unopened_auto_original_rig(60));
        rig.frame();
        let epoch = engine_epoch(&mut rig);
        pipeline_refuses_601(&mut rig);
        rig.frame();
        assert!(crate::route::flight_outstanding(), "the conversion's PMS half must be a recovery flight");
        rig.frames_until("the conversion's landing", |_| !crate::route::flight_outstanding());
        assert_eq!(rig.flight.decisions(), 1, "exactly one replacement /decision");
        let decisions = rig.flight.decision_requests();
        let replacement = decisions.first().expect("the replacement /decision");
        assert!(!replacement.contains("protocol=hls"), "the replacement is not the HLS fallback: {replacement}");
        assert!(!crate::route::is_segmented_hls(&rig.ps), "the route is not HLS");
        // The Original keeps its video bytes: the request still asks for a progressive stream with
        // the server's permission to COPY the video (`directStream=1`), which the HLS fallback's
        // encoded rendition never has (`directStream=0`), so only the audio is converted.
        assert!(
            replacement.contains("protocol=http") && replacement.contains("directStream=1"),
            "the Original's remux is not kept (the video copy is withdrawn): {replacement}"
        );
        assert_eq!(rig.ps.readout().original_failure(), None, "the Original did not fail, so no failure is noted");
        assert_ne!(engine_epoch(&mut rig), epoch, "the landing reloaded onto the converted route");
        assert_ne!(crate::player::state(&rig.ps), crate::player::PlaybackState::Error);
    }

    /// A conversion the server refuses ends on the read-out, once: the recovery flight flew (the
    /// frames between its dispatch and its landing kept the Engine and its spinner), the refusal
    /// asked for no further rung (a manual Original has no HLS contingency), and the route is still
    /// the one the pipeline refused. A failure before any picture holds nothing the television
    /// mistakes for playback, so [`release_failed_engine`](super::super::route_start::release_failed_engine)
    /// leaves the Engine to the viewer's Back.
    #[test]
    fn a_refused_conversion_ends_on_the_read_out_and_keeps_the_engine() {
        let mut rig = dts_rig(Rig::new(40));
        rig.flight.refuse_decisions(1);
        rig.frame();
        let epoch = engine_epoch(&mut rig);
        pipeline_refuses_601(&mut rig);
        rig.frame();
        assert!(crate::route::flight_outstanding(), "the conversion must be a flight, not an immediate failure");
        assert_ne!(crate::player::state(&rig.ps), crate::player::PlaybackState::Error, "the spinner while it flies");
        assert_eq!(engine_epoch(&mut rig), epoch, "frames between the dispatch and the landing keep the Engine");

        rig.frames_until("the refusal's landing", |r| crate::player::state(&r.ps) == crate::player::PlaybackState::Error);
        assert!(!crate::route::flight_outstanding());
        assert_eq!(rig.flight.decisions(), 1, "one conversion request, never a loop");
        assert_eq!(crate::route::transcode_session(&rig.ps), "rig-1", "the refused route is still the route");
        assert_eq!(engine_epoch(&mut rig), epoch, "a refusal reloads nothing");
        for _ in 0..3 {
            rig.frame();
        }
        assert_eq!(rig.flight.decisions(), 1, "the frames that follow dispatch no second flight");
        assert_eq!(crate::player::state(&rig.ps), crate::player::PlaybackState::Error);
        rig.flight.wait_for("the refused replacement's stop", |f| replacement_stops(f) == 1);
        assert!(!rig.flight.stopped().iter().any(|s| s == "rig-1"), "{:?}", rig.flight.stopped());
    }

    // ---- S2a: a refusal is learned only from a facet-clean proof ---------------------------------

    /// This thread plays the DTS set of [`dts_announcing_start`] on firmware `fw`, build `B`.
    fn firmware_announcing_dts() -> plx_platform::devcaps::CapsGuard {
        plx_platform::devcaps::firmware_on_this_thread_for_test("fw", "B", (*dts_announcing_start()).clone())
    }

    /// The rows the install's record holds, after the queued writes landed.
    fn recorded_refusals() -> Vec<plx_plex::plex::session::NativeAudioRefusal> {
        plx_base::storage_worker::drain_for_test();
        plx_plex::plex::session::peek().native_audio_refused().to_vec()
    }

    fn dts_row(proofs: u32) -> plx_plex::plex::session::NativeAudioRefusal {
        let dts6 = plx_platform::devcaps::Facet::Audio { codec: "dts".into(), channels: 6 };
        plx_plex::plex::session::NativeAudioRefusal::of_facet(&dts6, "fw", "B", Some(proofs)).expect("a counted row")
    }

    /// The reload the landing started decodes its first picture: the frame that says so. The hostsim
    /// sink refuses the reload's Load (nothing decodes in a host test); the pipeline this stands for
    /// took it ([`pipeline_takes_the_load`]).
    fn first_picture(rig: &mut Rig) {
        pipeline_takes_the_load(rig);
        plx_play_feed::ffi_host::force_clocksink_for_test(true);
        plx_play_feed::ffi_host::force_object_ready_for_test(true);
        FEED_SHARED.frames.store(1, Relaxed);
        rig.frame();
    }

    /// A DTS 601 whose conversion PMS answers video `copy`, audio `transcode`, and whose reload then
    /// shows a picture records ONE proof: stored and counted, not applied, so the next start still
    /// announces DTS 5.1.
    #[test]
    fn a_dts_601_rescued_by_its_conversion_records_one_proof_not_applied() {
        let mut rig = dts_rig(Rig::new(60));
        let _session = plx_plex::plex::session::TempSession::new("s2a-proof-one");
        let _set = firmware_announcing_dts();
        dts_601_converts_in_one_flight(&mut rig);
        assert!(recorded_refusals().is_empty(), "the landing alone proves nothing: the picture does");
        assert_eq!(rig.ps.open_recovery().phase(), super::super::recovery::Phase::Proving, "the clean landing is awaiting its picture");
        first_picture(&mut rig);
        assert_eq!(rig.ps.open_recovery().phase(), super::super::recovery::Phase::Playing, "the picture ended the open");
        assert_eq!(recorded_refusals(), [dts_row(1)]);
        assert!(!recorded_refusals()[0].applies());
        // The count-1 row is stored and counted, not applied: the record hands the play start nothing,
        // so the next start is the boot value itself.
        plx_plex::plex::session::save(&plx_plex::plex::session::peek());
        assert!(
            std::sync::Arc::ptr_eq(
                &plx_platform::devcaps::start_caps(&plx_plex::plex::session::peek().applied_refusals()),
                &plx_platform::devcaps::caps()
            ),
            "a once-proved row leaves the next start alone"
        );
        assert!(plx_platform::devcaps::caps().audio_supports("dts", 6), "one proof changes no capability answer");
    }

    /// A second rescued play persists the refusal; the next play's start value no longer announces
    /// DTS 5.1 (and PMS is told to bound it), while the boot snapshot is untouched.
    #[test]
    fn the_next_play_starts_without_a_twice_proved_dts_surround() {
        let mut rig = dts_rig(Rig::new(60));
        let session = plx_plex::plex::session::TempSession::new("s2a-proof-two");
        let _set = firmware_announcing_dts();
        dts_601_converts_in_one_flight(&mut rig);
        first_picture(&mut rig);
        assert_eq!(recorded_refusals(), [dts_row(1)]);

        // A second Play owns a fresh route request and finite offer catalog. Reusing the
        // converted Engine and merely resetting its picture counter would spend recovery in
        // the first request's budget, which is not a second Play. Preserve the first Play's
        // stored proof while closing its scratch session under the first rig's serial guard;
        // the second rig takes a new guard before opening its own scratch session.
        let remembered = plx_plex::plex::session::peek();
        drop(session);
        drop(rig);
        let mut rig = dts_rig(Rig::new(60));
        let _session = plx_plex::plex::session::TempSession::new("s2a-proof-two-next");
        plx_plex::plex::session::save(&remembered);
        assert_eq!(recorded_refusals(), [dts_row(1)], "the second Play begins with the persisted first proof");
        let epoch = engine_epoch(&mut rig).expect("the rig's Engine");
        plx_play_feed::sf_event::sf_on_event(epoch, 18, 601, std::ptr::null());
        rig.frame();
        rig.frames_until("the second conversion's landing", |_| !crate::route::flight_outstanding());
        first_picture(&mut rig);
        let rows = recorded_refusals();
        assert_eq!(rows, [dts_row(plx_plex::plex::session::LEARNED_REFUSAL_PROOFS)]);
        assert!(rows[0].applies());

        plx_plex::plex::session::save(&plx_plex::plex::session::peek());
        let dts6 = plx_platform::devcaps::Facet::Audio { codec: "dts".into(), channels: 6 };
        let start = plx_platform::devcaps::start_caps(&plx_plex::plex::session::peek().applied_refusals());
        assert!(!start.audio_supports("dts", 6), "the next start bounds DTS: {:?}", start.audio_channels);
        assert_eq!(start.without(&dts6), *start, "nothing left to take out");
        assert!(plx_platform::devcaps::caps().audio_supports("dts", 6), "the boot snapshot is untouched");
    }

    /// An HLS rescue (the both-facets rung) records nothing, however clean the verdicts.
    #[test]
    fn an_hls_rescued_play_records_nothing() {
        let mut rig = unopened_auto_original_rig(60);
        let _session = plx_plex::plex::session::TempSession::new("s2a-hls");
        let _set = firmware_announcing_dts();
        rig.flight.begin_attempt(&rig.ps);
        FlightRig::plan_on(&mut rig.ps, dts_announcing_start());
        FlightRig::declare_audio(&mut rig.ps, "aac", 2);
        rig.frame();
        pipeline_refuses_601(&mut rig);
        rig.frame();
        assert!(crate::route::flight_outstanding(), "the HLS contingency flies");
        rig.frames_until("the HLS landing", |_| !crate::route::flight_outstanding());
        assert!(crate::route::is_segmented_hls(&rig.ps), "the route is HLS now");
        first_picture(&mut rig);
        for _ in 0..20 {
            rig.frame();
        }
        assert_eq!(rig.ps.open_recovery().phase(), super::super::recovery::Phase::Playing, "the open ended in a picture");
        assert!(recorded_refusals().is_empty());
    }

    /// A Load timeout names no facet: the HLS contingency's rescue records nothing, DTS fed or not.
    #[test]
    fn a_timeout_rescued_play_records_nothing() {
        let mut rig = unopened_auto_original_rig(60);
        let _session = plx_plex::plex::session::TempSession::new("s2a-timeout");
        let _set = firmware_announcing_dts();
        rig.flight.begin_attempt(&rig.ps);
        declare_dts(&mut rig);
        rig.frame();
        FEED_SHARED.load_timed_out.store(true, Release);
        FEED_SHARED.load_failed.store(true, Release);
        rig.frame();
        assert!(crate::route::flight_outstanding(), "the HLS contingency flies");
        rig.frames_until("the HLS landing", |_| !crate::route::flight_outstanding());
        first_picture(&mut rig);
        for _ in 0..20 {
            rig.frame();
        }
        assert_eq!(rig.ps.open_recovery().phase(), super::super::recovery::Phase::Playing, "the open ended in a picture");
        assert!(recorded_refusals().is_empty());
    }

    /// PMS answered the conversion with the video transcoded too: the facet flight changed more
    /// than the refused facet, so the rescue records nothing.
    #[test]
    fn a_conversion_whose_video_was_transcoded_records_nothing() {
        let mut rig = dts_rig(Rig::new(60));
        let _session = plx_plex::plex::session::TempSession::new("s2a-video");
        let _set = firmware_announcing_dts();
        rig.flight.answer_decisions_with(crate::route::flight_rig::DECISION_TRANSCODES_BOTH_LANES);
        dts_601_converts_in_one_flight(&mut rig);
        assert_eq!(rig.ps.open_recovery().phase(), super::super::recovery::Phase::Opening, "the video was transcoded too: no proof is pending");
        first_picture(&mut rig);
        for _ in 0..5 {
            rig.frame();
        }
        assert_eq!(rig.ps.open_recovery().phase(), super::super::recovery::Phase::Playing, "the conversion landed and played");
        assert!(recorded_refusals().is_empty());
    }

    /// A refusal that names the codec is recorded in the step that asked, before any landing, and
    /// applies at once (`proofs` absent).
    #[test]
    fn a_named_opus_refusal_is_recorded_before_any_landing() {
        let mut rig = Rig::new(150);
        let _session = plx_plex::plex::session::TempSession::new("s2a-named");
        let _set = firmware_announcing_opus();
        rig.flight.begin_attempt(&rig.ps);
        FlightRig::plan_on(&mut rig.ps, plx_platform::devcaps::caps());
        FlightRig::declare_audio(&mut rig.ps, "opus", 6);
        rig.frame();
        let epoch = engine_epoch(&mut rig).expect("the rig's Engine");
        plx_play_feed::native_audio::publish_fed(plx_play_feed::native_audio::Kind::Opus, 6);
        rig.flight.hold_answers();
        plx_play_feed::sf_event::sf_on_event(epoch, 18, 200, std::ptr::null());
        rig.frame();
        assert!(crate::route::flight_outstanding(), "the conversion flight is outstanding");
        let rows = recorded_refusals();
        assert_eq!(rows.len(), 1, "{rows:?}");
        assert_eq!((rows[0].codec.as_str(), rows[0].channels, rows[0].proofs), ("opus", 6, None));
        assert!(rows[0].applies());
        rig.flight.release_answers();
        rig.frames_until("the conversion's landing", |_| !crate::route::flight_outstanding());
    }

    // ---- the machine across an Engine's reload, a park and a restore ----------------------------

    /// Home, then the foreground restore's Load of the same route: the Engine is torn down for
    /// reload (the request, its session and the route survive, an outstanding flight does not) and
    /// the next Engine is started on that route.
    fn park_and_restore(rig: &mut Rig) {
        suspend_bufferfeed(&mut rig.ps, &mut rig.pa);
        let restored = crate::player::start_bufferfeed_tracked(&mut rig.ps, &mut rig.pa);
        assert!(
            matches!(restored, super::super::engine::BufferfeedStartOutcome::Launched(_)),
            "the restore's Load did not start: {restored:?}"
        );
        pipeline_takes_the_load(rig);
    }

    /// Drain frames with no Engine until the late landing of a flight a park dropped is reaped (the
    /// run loop's per-frame `land_resume` does it), so no replacement encoder is left running.
    fn reap_the_dropped_flight(rig: &mut Rig) {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while replacement_stops(&rig.flight) == 0 {
            {
                let _frame = plx_base::task::FrameScope::enter();
                assert!(land_resume(&mut rig.ps, &mut rig.pa).is_none(), "a dropped flight landed");
            }
            assert!(std::time::Instant::now() < deadline, "timed out waiting for the dropped flight's reap");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// **A native-audio refusal on a RELOADED Engine converts, like the first Engine's.** The machine
    /// lives per request but a picture is per Engine (a reload clears `frames` and `seen_frame`): a
    /// manual Original shows its first picture, the viewer picks an Opus 5.1 track (a native audio
    /// reload), and the pipeline answers the new Engine's Load with `type=18 num=200`. The ladder
    /// converts that audio exactly as it did before the request's first picture; it used to read the
    /// request's history and end on the read-out for a film that main played converted.
    #[test]
    fn a_native_audio_refusal_on_a_reloaded_engine_after_the_first_picture_still_converts() {
        let mut rig = Rig::new(150);
        let _set = firmware_announcing_opus();
        rig.flight.begin_attempt(&rig.ps);
        FlightRig::plan_on(&mut rig.ps, plx_platform::devcaps::caps());
        FlightRig::declare_audio(&mut rig.ps, "opus", 6);
        FEED_SHARED.frames.store(1, Relaxed);
        rig.frame();
        assert_eq!(rig.ps.open_recovery().phase(), super::super::recovery::Phase::Playing, "sanity: the picture ended the first open");

        let epoch = engine_epoch(&mut rig);
        let reloaded = super::super::engine::reload_at(&mut rig.ps, &mut rig.pa, 60_000_000_000);
        assert_eq!(reloaded, super::super::engine::ReloadOutcome::Started);
        assert_ne!(engine_epoch(&mut rig), epoch, "sanity: the track pick reloaded the Engine");
        assert_eq!(FEED_SHARED.frames.load(Relaxed), 0, "sanity: the new Engine has shown no picture");
        let epoch = engine_epoch(&mut rig).expect("the reloaded Engine");

        plx_play_feed::native_audio::publish_fed(plx_play_feed::native_audio::Kind::Opus, 6);
        rig.flight.hold_answers();
        plx_play_feed::sf_event::sf_on_event(epoch, 18, 200, std::ptr::null());
        rig.frame();
        assert_ne!(
            crate::player::state(&rig.ps),
            crate::player::PlaybackState::Error,
            "a refused native audio track on a reloaded Engine ended on the read-out"
        );
        assert!(crate::route::flight_outstanding(), "the conversion must be a recovery flight");
        rig.flight.release_answers();
        rig.frames_until("the conversion's landing", |_| !crate::route::flight_outstanding());
        assert_eq!(rig.flight.decisions(), 1, "one conversion /decision");
        assert_ne!(engine_epoch(&mut rig), Some(epoch), "the landing reloaded onto the converted route");
        assert_ne!(crate::player::state(&rig.ps), crate::player::PlaybackState::Error);
    }

    /// Scenario A. A DTS 601 starts the facet flight; Home clears it (`begin_engine_teardown`), so
    /// the landing is discarded and the machine hears nothing; the foreground restore Loads the same
    /// refused route, which the pipeline refuses again. The restored Engine's refusal gets a rung
    /// from the SAME ladder (the facet is spent, one flight per request): never "Connecting" for
    /// good. Returns the rig after that frame.
    fn dts_refused_again_after_a_park_across_the_facet_flight(mut rig: Rig) -> Rig {
        rig.frame();
        pipeline_refuses_601(&mut rig);
        rig.frame();
        assert!(crate::route::flight_outstanding(), "sanity: the facet conversion is flying");
        rig.flight.wait_for("the conversion's request", |f| f.decisions() >= 1);
        suspend_bufferfeed(&mut rig.ps, &mut rig.pa);
        assert!(!crate::route::flight_outstanding(), "the park cleared the flight");
        reap_the_dropped_flight(&mut rig);
        let restored = crate::player::start_bufferfeed_tracked(&mut rig.ps, &mut rig.pa);
        assert!(matches!(restored, super::super::engine::BufferfeedStartOutcome::Launched(_)), "{restored:?}");
        pipeline_takes_the_load(&mut rig);
        rig.frame();
        assert_ne!(crate::player::state(&rig.ps), crate::player::PlaybackState::Error, "sanity: the restore re-opens");
        pipeline_refuses_601(&mut rig);
        rig.frame();
        rig
    }

    /// Scenario A, a manual Original: the restored Engine's refusal ends on the read-out.
    #[test]
    fn a_home_across_the_facet_flight_then_a_refused_restore_ends_on_the_read_out() {
        let rig = dts_refused_again_after_a_park_across_the_facet_flight(dts_rig(Rig::new(300)));
        assert_eq!(
            crate::player::state(&rig.ps),
            crate::player::PlaybackState::Error,
            "the restored Engine's refusal left the viewer on {:?}",
            crate::player::state(&rig.ps)
        );
        assert_eq!(rig.flight.decisions(), 1, "the facet is spent: no second conversion");
    }

    /// Scenario A, a cold Auto Original: the restored Engine's refusal takes the HLS contingency.
    #[test]
    fn a_home_across_the_facet_flight_then_a_refused_restore_takes_hls_for_a_cold_auto_original() {
        let mut rig = dts_refused_again_after_a_park_across_the_facet_flight(dts_rig(unopened_auto_original_rig(300)));
        assert!(
            crate::route::flight_outstanding(),
            "the restored Engine's refusal started no flight (state {:?})",
            crate::player::state(&rig.ps)
        );
        rig.frames_until("the HLS contingency's landing", |_| !crate::route::flight_outstanding());
        assert!(crate::route::is_segmented_hls(&rig.ps), "the route is HLS now");
        assert_ne!(crate::player::state(&rig.ps), crate::player::PlaybackState::Error);
    }

    /// Scenario A's other half: the restored Load SUCCEEDS. The machine must not stay in `Flight`
    /// for the rest of the request, where a later refusal of a reload would meet no rung.
    #[test]
    fn a_home_across_the_facet_flight_then_a_restore_that_opens_leaves_the_ladder_open() {
        let mut rig = dts_rig(Rig::new(300));
        rig.frame();
        pipeline_refuses_601(&mut rig);
        rig.frame();
        rig.flight.wait_for("the conversion's request", |f| f.decisions() >= 1);
        suspend_bufferfeed(&mut rig.ps, &mut rig.pa);
        reap_the_dropped_flight(&mut rig);
        let restored = crate::player::start_bufferfeed_tracked(&mut rig.ps, &mut rig.pa);
        assert!(matches!(restored, super::super::engine::BufferfeedStartOutcome::Launched(_)), "{restored:?}");
        assert_eq!(
            rig.ps.open_recovery().phase(),
            super::super::recovery::Phase::Opening,
            "the restored Load is a new open: the machine is not stuck in the flight the park dropped"
        );
    }

    /// Scenario B. A refused open with no suspect stream ends on the read-out (the machine is
    /// `Failed`; the Engine is not released, it never showed a picture); Home and the foreground
    /// restore Load the same route, and the pipeline refuses it again. The read-out shows again, not
    /// a silent spinner.
    #[test]
    fn a_home_over_the_read_out_then_a_refused_restore_shows_the_read_out_again() {
        let mut rig = Rig::new(40);
        rig.flight.begin_attempt(&rig.ps);
        FlightRig::plan_on(&mut rig.ps, dts_announcing_start());
        FlightRig::declare_audio(&mut rig.ps, "aac", 2);
        rig.frame();
        pipeline_refuses_601(&mut rig);
        rig.frame();
        assert_eq!(crate::player::state(&rig.ps), crate::player::PlaybackState::Error, "sanity: no rung, the read-out");
        park_and_restore(&mut rig);
        rig.frame();
        assert_ne!(crate::player::state(&rig.ps), crate::player::PlaybackState::Error, "sanity: the restore re-opens");
        pipeline_refuses_601(&mut rig);
        rig.frame();
        assert_eq!(
            crate::player::state(&rig.ps),
            crate::player::PlaybackState::Error,
            "the restored Engine's refusal left the viewer on {:?}",
            crate::player::state(&rig.ps)
        );
        assert_eq!(rig.flight.decisions(), 0, "no flight for a stream nothing can convert");
    }

    /// **The ABR climb after a DTS rescue does not propose DTS direct play again.** The cold Auto
    /// Original's stored candidate carries the DTS track and `feeds_part()`; the facet landing
    /// narrowed the attempt, and the candidate is re-graded against it, so a later Original
    /// recovery plans the remux (the server converting that audio), not the Part.
    #[test]
    fn the_climb_after_a_dts_rescue_plans_no_dts_direct_play() {
        let mut rig = auto_original_rig(60);
        rig.flight.begin_attempt(&rig.ps);
        declare_dts(&mut rig);
        FlightRig::offer_original_carrying(&mut rig.ps, true, "dca", 6);
        assert_eq!(FlightRig::original_feeds_part(&rig.ps), Some(true), "sanity: the candidate would feed the DTS Part");
        assert_eq!(FlightRig::original_recovery_is_remux(&rig.ps), Some(false));
        rig.frame();
        pipeline_refuses_601(&mut rig);
        rig.frame();
        assert!(crate::route::flight_outstanding(), "the conversion's PMS half must be a recovery flight");
        rig.frames_until("the conversion's landing", |_| !crate::route::flight_outstanding());
        assert_eq!(rig.flight.decisions(), 1);
        assert_eq!(FlightRig::original_feeds_part(&rig.ps), Some(false), "the Part's DTS is not offered again");
        assert_eq!(FlightRig::original_recovery_is_remux(&rig.ps), Some(true), "the climb plans the remux");
    }

    /// A refusal-recovery landing notes its delivery reason and the app's own recovery as the open's
    /// origin, and counts the attempt exactly once (the ordinal advances by one, not two).
    #[test]
    fn a_refusal_recovery_landing_notes_its_delivery_reason() {
        use plx_telemetry::telemetry::consent::{self, Consent};
        struct Restore(Option<Consent>);
        impl Drop for Restore {
            fn drop(&mut self) {
                super::super::report::reset_latches_for_test();
                consent::install(self.0.take().unwrap_or_default());
            }
        }
        let rig = Rig::new(60);
        let _restore = Restore(consent::current());
        let mut enabled = Consent::default();
        enabled.errors = true;
        enabled.asked_version = consent::POLICY_VERSION;
        consent::install(enabled);
        let mut rig = dts_rig(rig);
        rig.frame();
        pipeline_refuses_601(&mut rig);
        rig.frame();
        rig.frames_until("the conversion's landing", |_| !crate::route::flight_outstanding());
        let (reason, _) = super::super::report::last_delivery_requested_for_test().expect("the landing noted a delivery");
        assert_eq!(reason.code(), "refusal_recovery");
        let context = super::super::report::error_context_for_test(&rig.ps);
        assert_eq!(context.attempt_origin.code(), "recovery");
        assert_eq!(context.attempt_ordinal.code(), "2", "the open is counted once, as the second of the playback");
    }

    /// **`refuse601` publishes the firmware's refusal once per request, after the Load.** Armed
    /// (here in a private trigger root), the pump raises `type=18 num=601` through `sf_on_event`
    /// once the Load has returned and before any picture. The converted reload of the same request
    /// is not refused again, so the play reaches its picture.
    #[test]
    fn refuse601_publishes_once_per_request_after_the_load() {
        plx_base::devtrig::with_private_triggers(|| {
            std::fs::write(plx_base::devtrig::path("refuse601"), "").expect("arm the trigger");
            let mut rig = dts_rig(Rig::new_load_return_pending(60));
            let refusals = |log: &str| log.matches("Load refused by the pipeline before any picture (type=18 num=601)").count();
            let log = plx_base::eventlog::with_private_log(|| {
                let epoch = engine_epoch(&mut rig).expect("the rig's Engine");
                assert!(!FEED_SHARED.native_load_returned(epoch), "the callback preceded the worker's Load-return receipt");
                rig.frame();
                assert!(!FEED_SHARED.load_failed.load(Acquire), "nothing is published before the Load returned");
                assert!(FEED_SHARED.mark_native_load_returned(epoch, None).is_some(), "the Load returned");
                rig.frame();
                assert_eq!(FEED_SHARED.dg_cb_err_num.load(Relaxed), 601, "the refusal went through the callback");
                assert_eq!(rig.ps.attempt_view().facts().pipeline_error, Some(601),
                    "a refusal raised during the pump must reach its addressed view in the same frame");
                assert!(crate::route::flight_outstanding(), "the injected refusal ran the ladder: a conversion flies");
                rig.frames_until("the conversion's landing", |_| !crate::route::flight_outstanding());
                let reloaded = engine_epoch(&mut rig).expect("the reloaded Engine");
                assert_ne!(reloaded, epoch, "the landing reloaded the Engine");
                pipeline_takes_the_load(&mut rig);
                assert!(FEED_SHARED.mark_native_load_returned(reloaded, None).is_some(), "the reload's Load returned");
                for _ in 0..4 {
                    rig.frame();
                }
                std::fs::read_to_string(plx_base::eventlog::events_log()).unwrap_or_default()
            });
            assert_eq!(refusals(&log), 1, "one refusal for the request, none for its converted reload:\n{log}");
            assert_eq!(rig.flight.decisions(), 1, "one conversion flight, never a second");
            assert_ne!(crate::player::state(&rig.ps), crate::player::PlaybackState::Error);
        });
    }

    // ---- S4 enable: the app decodes what the pipeline refused --------------------------------------

    /// [`dts_announcing_start`] on a table that was read: the lossy policy admits DTS (it is measured).
    fn measured_dts_start() -> std::sync::Arc<plx_platform::devcaps::Caps> {
        std::sync::Arc::new(plx_platform::devcaps::Caps {
            app_lossy: plx_platform::devcaps::AppLossy::Measured,
            ..(*dts_announcing_start()).clone()
        })
    }

    /// `rig` plays a DTS 5.1 track natively, on a start value the lossy policy admits DTS on.
    fn measured_dts_rig(rig: Rig) -> Rig {
        measured_dts_rig_of(rig, 6)
    }

    /// [`measured_dts_rig`] for a DTS track of `channels` channels: the width decides whether the app's
    /// stereo decode is a downmix, and so where the rung sits in the ladder (and whether Force has it).
    fn measured_dts_rig_of(rig: Rig, channels: i64) -> Rig {
        let mut rig = rig;
        rig.flight.begin_attempt(&rig.ps);
        FlightRig::plan_on(&mut rig.ps, measured_dts_start());
        FlightRig::declare_audio(&mut rig.ps, "dca", channels);
        rig
    }

    /// A Force Direct Play rig playing a native DTS track of `channels` channels, on a start value the
    /// lossy policy admits DTS on.
    fn forced_dts_rig(channels: i64) -> Rig {
        let mut rig = Rig::direct(40);
        rig.flight.begin_attempt(&rig.ps);
        crate::route::set_direct_play_mode_for_test(&mut rig.ps, crate::route::DirectPlayMode::Forced);
        FlightRig::plan_on(&mut rig.ps, measured_dts_start());
        FlightRig::declare_audio(&mut rig.ps, "dca", channels);
        rig
    }

    const DECODING: &str = "open recovery: decoding dts in the app (stereo) after type=18 num=601";

    /// Drive a 601 on `rig` through the ladder until its flight (if any) has landed, returning the
    /// event log of the whole stretch. The Engine the rig started is `epoch`.
    fn refused_601_until_settled(rig: &mut Rig) -> String {
        plx_base::eventlog::with_private_log(|| {
            rig.frame();
            pipeline_refuses_601(rig);
            rig.frame();
            rig.frames_until("the ladder to settle", |_| !crate::route::flight_outstanding());
            std::fs::read_to_string(plx_base::eventlog::events_log()).unwrap_or_default()
        })
    }

    /// What the reload the app-decode rung asked for left: a new Engine, no failure read-out, and an
    /// attempt that declares the stream as the stereo PCM the app decodes.
    fn assert_decoding_in_the_app(rig: &mut Rig, epoch: Option<u32>, log: &str) {
        assert!(log.contains(DECODING), "{log}");
        assert!(log.contains("reload_at: fresh Load at"), "the same direct play reloads: {log}");
        assert_ne!(engine_epoch(rig), epoch, "the rung reloaded the Engine");
        assert_ne!(crate::player::state(&rig.ps), crate::player::PlaybackState::Error, "{log}");
        assert!(rig.ps.open_recovery().attempt().app_lossy_in_force.contains("dts"), "the attempt decodes DTS in the app");
        assert_eq!(
            crate::route::stream_pcm(&rig.ps).and_then(Result::ok),
            Some(crate::route::PcmFormat::STEREO_48K),
            "the reload's Load declares the PCM the app decodes"
        );
    }

    /// **P1. A DTS direct play whose conversion the server refuses is decoded in the app.** The pipeline
    /// refuses the native DTS (`601`), the facet rung asks the server to convert it, the server
    /// declines, and the last rung reloads the SAME direct play with the app decoding the audio, once,
    /// with no second conversion request (the source's video is not fed at a known raster here, so the
    /// manual encode is not earned: `a_refused_surround_dts_takes_the_manual_encode_before_the_app_decode`
    /// has the video). Before the rung existed the viewer got the read-out here.
    #[test]
    fn a_refused_dts_conversion_on_a_direct_play_decodes_in_the_app() {
        let mut rig = measured_dts_rig(Rig::direct(40));
        rig.flight.refuse_decisions(1);
        rig.frame();
        let epoch = engine_epoch(&mut rig);
        let log = refused_601_until_settled(&mut rig);
        assert_eq!(rig.flight.decisions(), 1, "the one conversion the server declined, and no other: {:?}", rig.flight.requests());
        assert_decoding_in_the_app(&mut rig, epoch, &log);
        assert_eq!(log.matches(DECODING).count(), 1, "{log}");
        assert!(!log.contains("no rung left"), "{log}");
        assert!(
            rig.ps.open_recovery().attempt().audio_supports("dts", 6),
            "the attempt announces what the app decodes, whatever the refused facet took out"
        );
    }

    /// **P2. A cold Auto Original that is a direct play reaches the app only after the HLS contingency
    /// is refused too**: two refused conversions (the facet rung's, then the HLS rung's), then the app,
    /// which plays the 5.1 track as stereo and is labelled so (the owner's last-rung downmix).
    #[test]
    fn a_cold_auto_original_decodes_in_the_app_only_after_hls_is_refused_too() {
        let mut rig = measured_dts_rig(Rig::direct_auto_original(40));
        rig.flight.refuse_decisions(2);
        rig.frame();
        let epoch = engine_epoch(&mut rig);
        let log = refused_601_until_settled(&mut rig);
        assert_eq!(rig.flight.decisions(), 2, "the facet rung's and the HLS rung's: {:?}", rig.flight.requests());
        assert!(rig.flight.decision_requests()[1].contains("hls"), "{:?}", rig.flight.decision_requests());
        assert_decoding_in_the_app(&mut rig, epoch, &log);
        assert!(!crate::route::is_segmented_hls(&rig.ps), "the route is still the direct play");
        assert!(!crate::route::is_transcoding(&rig.ps));
        // The last rung of Auto plays the 5.1 track as stereo, and the track menu says so.
        assert!(crate::route::audio_decoded_in_app(&rig.ps, "dca", 6, 0), "the track menu marks it played in stereo");
    }

    /// **P3. Force Direct Play decodes a refused native STEREO DTS in the app and asks the server NOTHING**:
    /// no `/decision` of any kind (not the conversion, not the probe), the same reload as above. (A
    /// surround track is not mixed down under Force: `a_forced_refusal_of_native_surround_dts_reads_out`.)
    #[test]
    fn a_forced_refusal_of_native_dts_decodes_in_the_app_with_no_server() {
        let mut rig = forced_dts_rig(2);
        assert_eq!(crate::route::stream_pcm(&rig.ps).and_then(Result::ok), None, "native until the pipeline refuses it");
        rig.frame();
        let epoch = engine_epoch(&mut rig);
        let log = refused_601_until_settled(&mut rig);
        assert_decoding_in_the_app(&mut rig, epoch, &log);
        assert!(
            !rig.flight.requests().iter().any(|r| r.contains("/decision")),
            "Force Direct Play converts nothing and asks nothing: {:?}",
            rig.flight.requests()
        );
        assert!(!crate::route::flight_outstanding());
    }

    /// **P3b. The rung's fresh Load is a PCM Load like any other, so it is planned its audio stack's lead.**
    /// `decode_in_app` reloads through `reload_at`, which starts the Engine through the one
    /// `start_bufferfeed_inner`: the reloaded Engine's payload declares `"audio":"PCM"` and its lead is the
    /// stack's (400 ms read from the output on the Load thread, pending until then, on `liblxa`; none on a
    /// stack that has none).
    #[test]
    fn the_app_decode_rungs_pcm_load_is_planned_its_stacks_lead() {
        use plx_platform::devcaps::{audio_stack_on_this_thread_for_test, AudioStack};
        for (stack, pending, lead_once_read) in
            [(AudioStack::Liblxa, true, 400_000_000), (AudioStack::Other, false, 0)]
        {
            let _stack = audio_stack_on_this_thread_for_test(stack);
            let mut rig = forced_dts_rig(2);
            rig.frame();
            let epoch = engine_epoch(&mut rig);
            let log = refused_601_until_settled(&mut rig);
            assert_decoding_in_the_app(&mut rig, epoch, &log);
            let eng = rig.pa.engine().expect("the reloaded Engine");
            let payload = eng.payload.to_string_lossy().into_owned();
            assert!(payload.contains(r#""audio":"PCM""#), "{stack:?}: the rung's Load declares PCM: {payload}");
            assert_eq!(eng.pcm_lead_pending, pending, "{stack:?}: the lead is read on the Load thread where the stack follows the output");
            assert_eq!(eng.pcm_sink_lead_ns, 0, "{stack:?}: nothing is applied before the Load returns");
            if pending {
                let epoch = eng.native_epoch;
                let deadline = std::time::Instant::now() + Duration::from_secs(5);
                while FEED_SHARED.native_load_pcm_lead(epoch).is_none() && std::time::Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(2));
                }
                assert_eq!(FEED_SHARED.native_load_pcm_lead(epoch), Some(lead_once_read), "{stack:?}: the Load thread's answer");
            }
        }
    }

    /// **P3c. Force Direct Play does NOT mix a surround track down** (owner decision 2026-10-28). The
    /// pipeline refuses the native DTS 5.1 (`601`) on a set whose policy admits DTS in the app, and the
    /// viewer gets the read-out: no app decode, no reload, no `/decision` of any kind. The 2.0 twin is
    /// `a_forced_refusal_of_native_dts_decodes_in_the_app_with_no_server`.
    #[test]
    fn a_forced_refusal_of_native_surround_dts_reads_out() {
        let mut rig = forced_dts_rig(6);
        rig.frame();
        let epoch = engine_epoch(&mut rig);
        let log = refused_601_until_settled(&mut rig);
        assert!(!log.contains("decoding dts in the app"), "{log}");
        assert!(log.contains("open recovery: no rung left"), "{log}");
        assert_eq!(crate::player::state(&rig.ps), crate::player::PlaybackState::Error, "{log}");
        assert_eq!(engine_epoch(&mut rig), epoch, "a refusal reloads nothing");
        assert!(!rig.ps.open_recovery().attempt().app_lossy_in_force.contains("dts"), "nothing decodes in the app");
        assert_eq!(crate::route::stream_pcm(&rig.ps).and_then(Result::ok), None, "the track stays native: no stereo PCM was declared");
        assert!(!crate::route::audio_decoded_in_app(&rig.ps, "dca", 6, 0), "and the track menu does not say it plays in stereo");
        assert!(
            !rig.flight.requests().iter().any(|r| r.contains("/decision")),
            "Force Direct Play converts nothing and asks nothing: {:?}",
            rig.flight.requests()
        );
    }

    /// **P3d. A surround track the server cannot convert takes the manual ENCODE before the app decode**
    /// (owner decision 2026-10-28: the app mixes a surround source down only as the very last rung). A
    /// DTS 5.1 direct play of a 1080p H.264: the `601` asks the server to convert the audio (declined),
    /// then to encode the video with the audio converted (declined), and only then does the app decode the
    /// audio, as stereo, labelled so in the track menu. Two `/decision`s, in that order.
    #[test]
    fn a_refused_surround_dts_takes_the_manual_encode_before_the_app_decode() {
        let mut rig = measured_dts_rig(Rig::direct(40));
        FlightRig::declare_video(&mut rig.ps, "h264", 1920, 1080);
        rig.flight.refuse_decisions(2);
        rig.frame();
        let epoch = engine_epoch(&mut rig);
        let log = refused_601_until_settled(&mut rig);
        assert_eq!(rig.flight.decisions(), 2, "the audio conversion and the encode, both declined: {:?}", rig.flight.requests());
        let facet = log.find("open recovery: converting Audio").unwrap_or_else(|| panic!("the facet rung flew first: {log}"));
        let both = log.find("open recovery: converting both facets").unwrap_or_else(|| panic!("the encode flew second: {log}"));
        let decode = log.find(DECODING).unwrap_or_else(|| panic!("the app decoded last: {log}"));
        assert!(facet < both && both < decode, "facet, then encode, then the app: {log}");
        assert_decoding_in_the_app(&mut rig, epoch, &log);
        assert_eq!(log.matches(DECODING).count(), 1, "{log}");
        assert!(crate::route::audio_decoded_in_app(&rig.ps, "dca", 6, 0), "labelled as played in stereo");
    }

    /// **P3e. A stereo track keeps the app decode BEFORE the manual encode** (today's order): the same
    /// rig as above with a 2.0 DTS track. The refused audio conversion is followed at once by the app
    /// decode (it keeps the direct play and loses no channel); the encode is not asked.
    #[test]
    fn a_refused_stereo_dts_is_decoded_in_the_app_before_the_manual_encode() {
        let mut rig = measured_dts_rig_of(Rig::direct(40), 2);
        FlightRig::declare_video(&mut rig.ps, "h264", 1920, 1080);
        rig.flight.refuse_decisions(2);
        rig.frame();
        let epoch = engine_epoch(&mut rig);
        let log = refused_601_until_settled(&mut rig);
        assert_eq!(rig.flight.decisions(), 1, "the audio conversion only; the encode waits behind the app: {:?}", rig.flight.requests());
        assert!(!log.contains("converting both facets"), "{log}");
        assert_decoding_in_the_app(&mut rig, epoch, &log);
        assert_eq!(log.matches(DECODING).count(), 1, "{log}");
    }

    /// **P4. A refused app decode ends on the read-out, once.** The reload's Load is refused too: the rung
    /// is spent and no facet is left, so the Engine fails, the read-out shows, and nothing reloads again.
    #[test]
    fn a_refused_app_decode_ends_on_the_read_out_once() {
        let mut rig = forced_dts_rig(2);
        rig.frame();
        let epoch = engine_epoch(&mut rig);
        let first = refused_601_until_settled(&mut rig);
        assert_decoding_in_the_app(&mut rig, epoch, &first);
        let reloaded = engine_epoch(&mut rig).expect("the reloaded Engine");
        pipeline_takes_the_load(&mut rig);
        assert!(FEED_SHARED.mark_native_load_returned(reloaded, None).is_some(), "the reload's Load returned");
        let second = plx_base::eventlog::with_private_log(|| {
            plx_play_feed::sf_event::sf_on_event(reloaded, 18, 601, std::ptr::null());
            for _ in 0..4 {
                rig.frame();
            }
            std::fs::read_to_string(plx_base::eventlog::events_log()).unwrap_or_default()
        });
        let log = format!("{first}{second}");
        assert_eq!(crate::player::state(&rig.ps), crate::player::PlaybackState::Error, "{log}");
        assert_eq!(log.matches(DECODING).count(), 1, "the rung runs once: {log}");
        assert_eq!(log.matches("open recovery: no rung left").count(), 1, "the read-out is asked for once: {log}");
        assert_eq!(log.matches("reload_at: fresh Load at").count(), 1, "no second reload: {log}");
        assert!(rig.flight.requests().iter().all(|r| !r.contains("/decision")), "{:?}", rig.flight.requests());
    }

    /// **P5. A refused conversion of a remux still ends on the read-out**: the app decode is for a
    /// direct play only (the pin of the scope cut: a remux's audio is the server's copy, and a rebuild
    /// flight for it is a separate design).
    #[test]
    fn a_refused_conversion_of_a_remux_still_ends_on_the_read_out() {
        let mut rig = measured_dts_rig(Rig::new(40));
        rig.flight.refuse_decisions(1);
        rig.frame();
        let epoch = engine_epoch(&mut rig);
        let log = plx_base::eventlog::with_private_log(|| {
            rig.frame();
            pipeline_refuses_601(&mut rig);
            rig.frame();
            rig.frames_until("the refusal's landing", |r| crate::player::state(&r.ps) == crate::player::PlaybackState::Error);
            std::fs::read_to_string(plx_base::eventlog::events_log()).unwrap_or_default()
        });
        assert!(!log.contains("decoding dts in the app"), "{log}");
        assert!(log.contains("open recovery: no rung left"), "{log}");
        assert_eq!(engine_epoch(&mut rig), epoch, "a refusal reloads nothing");
        assert_eq!(rig.flight.decisions(), 1);
        assert!(!rig.ps.open_recovery().attempt().app_lossy_in_force.contains("dts"));
    }

    /// **P6. A refused APP-DECODED Load that the server then converts proves nothing.** The in-app decode
    /// is on (the debug policy puts DTS in force), the Load of its PCM is refused, no audio is the
    /// suspect (the pipeline decoded none of it) so the 4K H.264 is, and the server's conversion lands
    /// clean: the reload's first picture must not write a refusal of the set's own DTS decoder, which
    /// this Load never exercised.
    #[test]
    fn a_refused_app_decoded_load_converted_by_the_server_proves_nothing() {
        let mut rig = Rig::direct(40);
        let _session = plx_plex::plex::session::TempSession::new("s4-app-decoded-no-proof");
        let _set = firmware_announcing_dts();
        rig.flight.begin_attempt(&rig.ps);
        FlightRig::plan_on(&mut rig.ps, std::sync::Arc::new((*dts_announcing_start()).clone().with_app_lossy()));
        FlightRig::declare_audio(&mut rig.ps, "dca", 6);
        FlightRig::declare_video(&mut rig.ps, "h264", 3840, 2160);
        assert!(crate::route::stream_audio(&rig.ps).is_some_and(|a| a.app_decoded_lossy), "sanity: the app decodes it");
        rig.frame();
        let epoch = engine_epoch(&mut rig);
        let log = refused_601_until_settled(&mut rig);
        assert_eq!(rig.flight.decisions(), 1, "the video's conversion flew: {:?}", rig.flight.requests());
        assert!(!log.contains("decoding dts in the app"), "{log}");
        assert_ne!(engine_epoch(&mut rig), epoch, "the landing reloaded onto the conversion");
        first_picture(&mut rig);
        assert_eq!(rig.ps.open_recovery().phase(), super::super::recovery::Phase::Playing);
        assert!(recorded_refusals().is_empty(), "nothing about the set's DTS decoder was proved: {:?}", recorded_refusals());
    }
}

/// #568, at the PUMP level: a direct-play seek on a `VP_EXPORTED` (webOS 5+) session, driven by
/// the real `pump` ticks and the real `feed_stream` against the host sink with the clock sink
/// armed (which reports `VP_EXPORTED`). `reload_at` has no route to reload in a unit rig (an empty
/// URL is ignored), so each call is read off `engine::RELOAD_AT_LOG`; the rig stands in for the
/// reload's EFFECT (a new Engine) where the next seek needs one.
#[cfg(all(test, feature = "hostsim"))]
mod native_seek_pump_tests {
    use std::sync::atomic::Ordering::Relaxed;
    use super::*;
    use plx_play_feed::shared::FEED_TX;
    use plx_play_feed::ffi_host;
    use plx_playback_contract::feed::SeekMode;
    use std::os::raw::c_int;

    pub(super) const TARGET_NS: i64 = 905_000_000_000;
    const NEXT_NS: i64 = 1_200_000_000_000;
    pub(super) const V_STEP_NS: i64 = 41_708_333;

    pub(super) struct Rig {
        pub(super) ps: crate::route::PlaybackSession,
        pub(super) pa: super::super::adapter::PlayerAdapter,
        pub(super) now: u32,
    }

    impl Rig {
        pub(super) fn new(force_seek: Option<c_int>) -> Rig {
            crate::player::reset_session_for_test();
            FEED_TX.reset();
            super::super::claim_hold::clear();
            ffi_host::reset_native_lifecycle_for_test();
            ffi_host::force_clocksink_for_test(true);
            ffi_host::force_object_ready_for_test(true);
            ffi_host::reset_seek_probe_for_test(force_seek);
            super::super::engine::RELOAD_AT_LOG.lock().unwrap_or_else(|e| e.into_inner()).clear();
            super::super::engine::RELOAD_PREROLL_LOG.lock().unwrap_or_else(|e| e.into_inner()).clear();
            plx_play_feed::seek::set_native_seek_refused(false);
            let mut pa = super::super::adapter::PlayerAdapter::new_probed(unsafe { MainThread::assume() });
            let epoch = FEED_SHARED.begin_native_session().expect("native session");
            let mut eng = super::super::engine::streaming_engine_for_test(epoch);
            eng.stage = Stage::Playing;
            pa.install(eng);
            FEED_SHARED.duration_ns.store(7_200_000_000_000, Relaxed);
            FEED_SHARED.file_size.store(50_000_000_000, Relaxed);
            FEED_SHARED.playpos_ns.store(60_000_000_000, Relaxed);
            plx_play_feed::seek::set_seek_mode(SeekMode::NativeSeek);
            // The pump's `present` addresses the attempt the session's view began for, and the readout
            // it feeds is what `player::state` reads: the rig owes it that attempt.
            let mut ps = crate::route::PlaybackSession::IDLE;
            ps.begin_attempt_view(crate::route::RouteStartAttempt::fixture(), super::super::attempt_view::Backend::Feed);
            Rig { ps, pa, now: 1_000 }
        }

        pub(super) fn frame(&mut self) {
            let _frame = plx_base::task::FrameScope::enter();
            self.now += 16;
            pump(&mut self.ps, &mut self.pa, self.now);
            // Every writer steps both copies of the transport: a frame that left them apart is a
            // missed site.
            crate::player::assert_transport_copies_agree(&self.ps);
        }

        pub(super) fn eng(&mut self) -> std::cell::RefMut<'_, Engine> {
            self.pa.engine().expect("engine")
        }

        /// The demux thread's job: the first keyframe at `ns` and the AUs after it.
        fn land_keyframe(&mut self, ns: i64) {
            self.land_keyframe_aus(ns, 60);
        }

        /// [`Self::land_keyframe`] with `n` AUs: the prime wants `PRIME_VIDEO_MAX_NS` (2.5 s) of
        /// accepted video on an audioless stream, which is 61 of these.
        fn land_keyframe_aus(&mut self, ns: i64, n: i64) {
            self.land_keyframe_with_key(ns, n, 1);
        }

        /// [`Self::land_keyframe_aus`] whose first access unit carries `first_key` (the AU's `key`:
        /// `1` an IDR or any plain keyframe, `21` an HEVC CRA, see `plx_play_feed::aq::AuNode`).
        pub(super) fn land_keyframe_with_key(&mut self, ns: i64, n: i64, first_key: c_int) {
            // The demux thread took the av_seek mailbox before it produced the keyframe.
            FEED_SHARED.seek_to_ns.store(-1, Release);
            let au = [0u8; 64];
            let qv = &mut **self.eng().aq_video.as_mut().unwrap() as *mut plx_play_feed::aq::AuQueue;
            for i in 0..n {
                let key = if i == 0 { first_key } else { 0 };
                plx_play_feed::aq::aq_push(qv, au.as_ptr(), au.len() as c_int, ns + i * V_STEP_NS, key, 1);
            }
        }

        /// Request a seek and frame until the pump has armed it (flushed, awaiting the keyframe).
        pub(super) fn arm(&mut self, ns: i64) {
            crate::player::request_seek(&mut self.ps, &mut self.pa, ns);
            for _ in 0..20 {
                self.frame();
                if self.eng().flushed {
                    return;
                }
            }
            panic!("the pump never armed the seek");
        }
    }

    impl Drop for Rig {
        fn drop(&mut self) {
            plx_play_feed::seek::set_native_seek_refused(false);
            plx_play_feed::seek::set_seek_mode(SeekMode::Segment);
            ffi_host::reset_seek_probe_for_test(None);
            ffi_host::reset_native_lifecycle_for_test();
            ffi_host::force_clocksink_for_test(false);
            ffi_host::force_object_ready_for_test(false);
            FEED_SHARED.test_force_native_idle();
            crate::player::reset_session_for_test();
            FEED_TX.reset();
            crate::route::reset_player_control_for_test(crate::route::idle_session_for_test());
        }
    }

    pub(super) fn reloads() -> Vec<i64> {
        super::super::engine::RELOAD_AT_LOG.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    #[test]
    fn a_primed_segment_seek_with_only_stale_clock_samples_reloads_once() {
        let _serial = plx_base::testlock::serial();
        let mut rig = Rig::new(None);
        plx_play_feed::seek::set_seek_mode(SeekMode::Segment);
        FEED_SHARED.playpos_ns.store(2_220_000_000_000, Relaxed);
        let target = 2_118_000_000_000;
        let keyframe = 2_112_110_000_000;
        rig.arm(target);
        rig.land_keyframe_aus(keyframe, 80);
        let epoch = rig.eng().native_epoch;
        for _ in 0..20 {
            rig.frame();
            if !rig.eng().prime_play {
                break;
            }
        }
        assert!(!rig.eng().prime_play, "the in-place seek reached Play");
        assert!(ffi_host::segment_calls_for_test() > 0, "the new segment was sent");
        // A displayed-count callback and the clock on the retired timeline raced the seek.
        plx_play_feed::sf_event::sf_on_event(epoch, 47, 9, std::ptr::null());
        plx_play_feed::sf_event::sf_on_event(epoch, 0, 9_000_000_000, std::ptr::null());
        rig.frame();
        assert_eq!(rig.ps.readout().raw().playpos_ns, 9_000_000_000, "the stale position reached the pump");
        assert!(rig.ps.readout().seeking(), "the segment is still awaiting presentation");
        assert!(super::super::timeline::TimelineSnapshot::from_feed(
            rig.ps.readout(), 7_200_000_000_000,
        ).is_none(), "Plex must not save the stale clock");
        assert!(reloads().is_empty(), "presentation gets time to land");
        rig.now += 10_000;
        for _ in 0..5 {
            rig.frame();
        }
        assert_eq!(reloads(), vec![target], "reopen succeeded, but the seek needs one fresh Load");
    }

    #[test]
    fn an_old_segment_deadline_does_not_reload_a_newer_seek() {
        let _serial = plx_base::testlock::serial();
        let mut rig = Rig::new(None);
        plx_play_feed::seek::set_seek_mode(SeekMode::Segment);
        rig.arm(TARGET_NS);
        rig.land_keyframe_aus(TARGET_NS, 80);
        for _ in 0..20 {
            rig.frame();
        }
        assert!(!rig.eng().prime_play, "the first seek reached Play");
        rig.now += 10_000;
        rig.arm(NEXT_NS);
        assert_eq!(FEED_SHARED.seek_to_ns.load(Relaxed), NEXT_NS);
        assert!(reloads().is_empty(), "the newer seek got its own demux and presentation budgets");
    }

    #[test]
    fn a_native_seek_lands_on_the_keyframe_without_a_flush_or_a_reload() {
        let _serial = plx_base::testlock::serial();
        let mut rig = Rig::new(None);
        rig.arm(TARGET_NS);
        assert_eq!(FEED_SHARED.seek_to_ns.load(Relaxed), TARGET_NS, "the demux thread is told to av_seek");
        rig.land_keyframe(TARGET_NS);
        for _ in 0..5 {
            rig.frame();
        }
        assert_eq!(ffi_host::seek_log_for_test(), vec![TARGET_NS / 1_000_000], "one native Seek, at the keyframe");
        assert_eq!(ffi_host::flush_calls_for_test(), 0, "Seek flushes itself; the pump must not");
        assert_eq!(ffi_host::segment_calls_for_test(), 0, "the private Segment verbs never run on VP_EXPORTED");
        assert!(reloads().is_empty(), "no fresh Load");
        assert!(!rig.eng().flushed && !rig.eng().rebase_pending, "the seek is applied");
        assert_eq!(plx_play_feed::seek::seek_mode(), SeekMode::NativeSeek);
    }

    #[test]
    fn a_refused_seek_reloads_exactly_once_at_the_target_even_past_the_stuck_watchdog() {
        let _serial = plx_base::testlock::serial();
        let mut rig = Rig::new(Some(0));
        rig.arm(TARGET_NS);
        rig.land_keyframe(TARGET_NS);
        rig.frame(); // the feed refuses the Seek and re-posts the target for the reload
        assert_eq!(ffi_host::seek_log_for_test().len(), 1, "one refused Seek");
        assert_eq!(FEED_TX.seek_to_ns.load(Relaxed), TARGET_NS, "re-posted for the reload");
        assert_eq!(rig.ps.readout().pending_seek(), Some(TARGET_NS),
            "the player adopted the feed's request once its sample was ingested");
        // Late in the window: the stuck watchdog (SEEK_STUCK_MS) is due on the very next tick, and
        // it runs BEFORE the pending-seek branch.
        rig.now += 5_000;
        for _ in 0..6 {
            rig.frame();
            if !reloads().is_empty() {
                break;
            }
        }
        assert_eq!(ffi_host::seek_log_for_test().len(), 1, "one refused Seek");
        assert_eq!(reloads(), vec![TARGET_NS], "exactly one reload_at, at the user's target");
        assert_eq!(
            FEED_SHARED.seek_to_ns.load(Relaxed),
            -1,
            "the demux feed is not re-opened with a second av_seek in Reload mode"
        );
        assert_eq!(plx_play_feed::seek::seek_mode(), SeekMode::Reload);
        assert!(plx_play_feed::seek::native_seek_refused());

        // Another keyframe reaching the flushed engine must not anchor through the private offsets.
        rig.land_keyframe(TARGET_NS + 1_000_000_000);
        for _ in 0..3 {
            rig.eng(); // the real reload replaced the engine; here it is the same, still flushed
            let mt = unsafe { MainThread::assume() };
            super::super::engine::feed_both_lanes(&mt, &mut rig.eng());
        }
        assert_eq!(ffi_host::segment_calls_for_test(), 0, "no setTimeToDecode/setContentInfo/sendSegment, ever");
        assert_eq!(ffi_host::flush_calls_for_test(), 0);
    }

    #[test]
    fn once_refused_the_same_playback_reloads_every_later_seek_without_calling_seek() {
        let _serial = plx_base::testlock::serial();
        let mut rig = Rig::new(Some(0));
        rig.arm(TARGET_NS);
        rig.land_keyframe(TARGET_NS);
        for _ in 0..3 {
            rig.frame();
        }
        assert_eq!(ffi_host::seek_log_for_test().len(), 1);
        assert_eq!(reloads(), vec![TARGET_NS]);

        // The reload starts a NEW session in the same playback: it must not re-offer the Seek.
        let mode = plx_play_feed::seek::session_seek_mode(true, false, crate::player::VP_EXPORTED, None);
        assert_eq!(mode, SeekMode::Reload, "the downgrade survives reload_at");
        plx_play_feed::seek::set_seek_mode(mode);
        // Stand in for the reload's new Engine.
        let epoch = FEED_SHARED.begin_native_session().unwrap_or(0);
        let mut fresh = super::super::engine::streaming_engine_for_test(epoch);
        fresh.stage = Stage::Playing;
        rig.pa.install(fresh);
        crate::player::request_seek(&mut rig.ps, &mut rig.pa, NEXT_NS);
        for _ in 0..4 {
            rig.frame();
        }
        assert_eq!(reloads(), vec![TARGET_NS, NEXT_NS], "the next seek is a reload");
        assert_eq!(ffi_host::seek_log_for_test().len(), 1, "Seek is not called again this playback");
        assert_eq!(ffi_host::segment_calls_for_test(), 0);

        // A fresh play is a new playback and gets to try the native Seek again.
        let _ = crate::player::start_bufferfeed_tracked(&mut rig.ps, &mut super::super::adapter::PlayerAdapter::new_fed(unsafe { MainThread::assume() }));
        assert!(!plx_play_feed::seek::native_seek_refused(), "a fresh play clears the latch");
    }

    /// The one-shot reload a speed change arms lives in the session's readout: it turns the next
    /// in-place seek into a full reload, and the pump takes it with that seek, so the one after
    /// lands in place again.
    #[test]
    fn an_armed_rate_reload_turns_the_next_in_place_seek_into_one_reload_and_is_taken_once() {
        use crate::player::readout::ReadoutEvent;
        let _serial = plx_base::testlock::serial();
        let mut rig = Rig::new(None);
        rig.ps.step_readout(ReadoutEvent::RateReloadArmed);
        assert!(rig.ps.readout().rate_reload());
        crate::player::request_seek(&mut rig.ps, &mut rig.pa, TARGET_NS);
        for _ in 0..6 {
            rig.frame();
            if !reloads().is_empty() {
                break;
            }
        }
        assert_eq!(reloads(), vec![TARGET_NS], "a native-seek mode still reloads the seek that carries a speed");
        assert!(ffi_host::seek_log_for_test().is_empty(), "no in-place Seek");
        assert!(!rig.ps.readout().rate_reload(), "the pump took the one-shot with that seek");

        // Taken exactly once: what the pump asks of the NEXT seek (a native-seek mode, 1x, nothing
        // armed) is an ordinary in-place seek.
        assert!(!super::seek_must_reload(plx_play_feed::seek::seek_mode(),
            crate::player::play_rate_permille(&rig.ps), rig.ps.readout().rate_reload()),
            "the reload was one-shot");
    }

    // ---- a seek armed under a viewer Pause is owned by the media layer (a bare request_seek) ----

    impl Rig {
        /// The viewer pauses through the real path; the HLS clock is fresh so the Pause is accepted.
        pub(super) fn viewer_pause(&mut self) {
            FEED_SHARED.reset_hls_clock_for_test();
            assert!(crate::player::pause(&mut self.ps, &mut self.pa), "viewer Pause accepted");
            assert!(FEED_TX.paused.load(Acquire));
        }
    }

    fn viewer_paused_seek_lands_in_place(mode: SeekMode) {
        let _serial = plx_base::testlock::serial();
        let mut rig = Rig::new(None);
        plx_play_feed::seek::set_seek_mode(mode);
        rig.viewer_pause();
        rig.arm(TARGET_NS); // a bare request_seek, as the dev autoseek trigger does
        assert!(FEED_TX.seek_preroll_active(), "the pump opened the paused-seek preroll itself");
        assert!(rig.ps.readout().feed_allowed() && FEED_TX.feed_allowed(),
            "the gate the pump's watchdog reads is open for the preroll's keyframe, in both copies");
        rig.land_keyframe_aus(TARGET_NS, 80);
        for _ in 0..20 {
            rig.frame();
        }
        assert!(!rig.eng().flushed && !rig.eng().rebase_pending, "the keyframe reached feed_stream's rebase");
        assert!(!rig.eng().prime_play, "the preroll clock started");
        match mode {
            SeekMode::NativeSeek => {
                assert_eq!(ffi_host::seek_log_for_test(), vec![TARGET_NS / 1_000_000], "one Seek, at the keyframe");
            }
            _ => assert!(ffi_host::segment_calls_for_test() > 0, "the segment was anchored on the keyframe"),
        }
        // The sink and callback clock must both follow a Segment seek before the paused
        // preroll closes. NativeSeek continues to use its own landing semantics.
        if mode == SeekMode::Segment {
            let epoch = rig.eng().native_epoch;
            plx_play_feed::sf_event::sf_on_event(epoch, 47, 1, std::ptr::null());
            plx_play_feed::sf_event::sf_on_event(epoch, 0, TARGET_NS, std::ptr::null());
        }
        rig.now += 5_000; // well past the reopen budget
        for _ in 0..3 {
            rig.frame();
        }
        assert!(reloads().is_empty(), "no stuck-watchdog reload");
        // The first post-seek frame is presented: the pump re-pauses and restores the viewer hold.
        FEED_SHARED.frames.store(1, Relaxed);
        rig.frame();
        assert!(!FEED_TX.seek_preroll_active(), "the preroll closed on the landing");
        assert!(FEED_TX.paused.load(Acquire), "the viewer is still paused");
        assert!(!rig.ps.readout().feed_allowed() && !FEED_TX.feed_allowed(), "and the gate is shut again behind it");
        assert!(!ffi_host::clock_state_for_test().1, "the physical clock is held again");
        assert!(reloads().is_empty());
    }

    #[test]
    fn a_bare_native_seek_while_viewer_paused_lands_in_place_and_ends_paused() {
        viewer_paused_seek_lands_in_place(SeekMode::NativeSeek);
    }

    #[test]
    fn a_bare_segment_seek_while_viewer_paused_lands_in_place_and_ends_paused() {
        viewer_paused_seek_lands_in_place(SeekMode::Segment);
    }

    #[test]
    fn a_bare_paused_request_seek_is_owned_by_the_dispatcher_not_the_mid_seek_branch() {
        let _serial = plx_base::testlock::serial();
        let mut rig = Rig::new(None);
        rig.viewer_pause();
        FEED_SHARED.seek_target_ns.store(123_000_000_000, Relaxed); // a stale target from an earlier seek
        MID_SEEK_CLAIMS.store(0, Relaxed);
        crate::player::request_seek(&mut rig.ps, &mut rig.pa, TARGET_NS);
        rig.frame();
        assert_eq!(
            MID_SEEK_CLAIMS.load(Relaxed),
            0,
            "a seek not yet dispatched is not a seek the viewer paused mid-flight"
        );
        assert_eq!(FEED_TX.owned_paused_seek(), Some(TARGET_NS), "first owned at the request's own target");
    }

    #[test]
    fn a_pause_pressed_mid_flight_takes_the_mid_seek_branch() {
        let _serial = plx_base::testlock::serial();
        let mut rig = Rig::new(None);
        rig.arm(TARGET_NS);
        MID_SEEK_CLAIMS.store(0, Relaxed);
        assert!(crate::player::pause(&mut rig.ps, &mut rig.pa));
        rig.frame();
        assert_eq!(MID_SEEK_CLAIMS.load(Relaxed), 1);
    }

    #[test]
    fn a_gate_shut_under_an_armed_seek_is_reopened_and_the_watchdog_counts_from_the_reopen() {
        let _serial = plx_base::testlock::serial();
        let mut rig = Rig::new(None);
        rig.viewer_pause();
        rig.arm(TARGET_NS);
        crate::player::finish_seek_preroll(&mut rig.ps, &mut rig.pa); // something closed the preroll while the keyframe was still owed
        assert!(!FEED_TX.feed_allowed());
        rig.now += 5_000; // far past SEEK_STUCK_MS
        rig.frame();
        assert!(FEED_TX.feed_allowed(), "the pump reopens the gate rather than retry or reload into it");
        assert!(reloads().is_empty(), "no reload on the tick the gate reopened");
        assert_eq!(FEED_SHARED.seek_to_ns.load(Relaxed), TARGET_NS, "nor a retry re-publishing the av_seek");
        for _ in 0..3 {
            rig.frame();
        }
        assert!(reloads().is_empty(), "the clock was re-based by the reopen");
        rig.now += 5_000;
        rig.frame();
        assert_eq!(FEED_SHARED.seek_to_ns.load(Relaxed), TARGET_NS, "first stuck retry once the feed was open");
    }

    #[test]
    fn the_pump_owned_preroll_dies_with_the_playback() {
        let _serial = plx_base::testlock::serial();
        let mut rig = Rig::new(None);
        rig.viewer_pause();
        rig.arm(TARGET_NS);
        assert_eq!(FEED_TX.owned_paused_seek(), Some(TARGET_NS));
        // a stop: the feed's reset by scope clears the transport, the route steps the readout's copy
        rig.pa.reset_session(plx_playback_contract::feed::ResetScope::Stop, &mut || {});
        super::super::route_start::stop_transport(&mut rig.ps);
        assert!(FEED_TX.owned_paused_seek().is_none() && !FEED_TX.seek_preroll_active());
        assert!(!rig.ps.readout().paused() && !rig.ps.readout().seek_preroll(), "the readout's too");
        crate::player::assert_transport_copies_agree(&rig.ps);
        // …and a Resume that closes the preroll drops the ownership with it.
        crate::player::commit_paused(&mut rig.ps, &mut rig.pa, true);
        own_seek_under_pause(&mut rig.ps, &mut rig.pa, 1);
        assert!(FEED_TX.owned_paused_seek().is_some());
        crate::player::finish_seek_preroll(&mut rig.ps, &mut rig.pa);
        assert!(FEED_TX.owned_paused_seek().is_none());
        crate::player::assert_transport_copies_agree(&rig.ps);
    }

    #[test]
    fn a_preroll_a_caller_opened_stays_that_callers_to_close() {
        let _serial = plx_base::testlock::serial();
        let mut rig = Rig::new(None);
        rig.viewer_pause();
        crate::player::begin_paused_seek(&mut rig.ps, &mut rig.pa); // the foreground restore opens its own
        assert!(FEED_TX.seek_preroll_active() && rig.ps.readout().seek_preroll(), "the restore's preroll is open");
        rig.arm(TARGET_NS);
        assert!(FEED_TX.owned_paused_seek().is_none(), "the caller's preroll is not the pump's");
        rig.land_keyframe(TARGET_NS);
        for _ in 0..5 {
            rig.frame();
        }
        FEED_SHARED.frames.store(1, Relaxed);
        rig.frame();
        assert_eq!(ffi_host::seek_log_for_test(), vec![TARGET_NS / 1_000_000]);
        assert!(FEED_TX.seek_preroll_active(), "the pump leaves a caller's preroll for the caller");
    }

    /// (a) The feed opens the preroll inside `paused_seek_step` and can report `finish` in the SAME
    /// call. The readout steps the open from the verdict before `finish_paused_seek` reads it, so the
    /// re-pause runs and closes both copies: a level sampled at the end of the pump would still read
    /// "no preroll" there, `finish_paused_seek` would return early and the film would play on under
    /// a Paused transport (the feed's preroll left open, the readout Paused with none).
    #[test]
    fn a_step_that_opens_the_preroll_and_finishes_it_in_one_call_leaves_the_film_paused() {
        let _serial = plx_base::testlock::serial();
        let mut rig = Rig::new(None);
        rig.viewer_pause();
        assert!(FEED_TX.own_paused_seek(0), "the feed opens the preroll inside its step");
        assert!(!rig.ps.readout().seek_preroll(), "premise: nothing has stepped the readout yet");
        super::apply_paused_seek_step(&mut rig.ps, &mut rig.pa, plx_playback_contract::feed::PausedSeekStep {
            finish: true, mid_seek_claimed: false, preroll_opened: true,
        });
        assert!(!FEED_TX.seek_preroll_active(), "the preroll the feed opened is closed by the re-pause");
        assert!(FEED_TX.owned_paused_seek().is_none());
        assert!(!rig.ps.readout().seek_preroll(), "and the readout's copy with it");
        assert!(rig.ps.readout().paused() && FEED_TX.paused.load(Acquire), "the film stays paused");
        crate::player::assert_transport_copies_agree(&rig.ps);
    }

    /// (b), through the real call: the stop of a live playback retires the viewer's Pause, the
    /// preroll and the re-pause the foreground restore owed from BOTH copies of the transport (a
    /// reload keeps them: every reload test of this module ends with the same agreement check).
    #[test]
    fn a_real_stop_clears_the_pause_the_preroll_and_the_owed_repause_in_both_copies() {
        let _serial = plx_base::testlock::serial();
        let mut rig = Rig::new(None);
        rig.viewer_pause();
        crate::player::begin_paused_seek(&mut rig.ps, &mut rig.pa);
        crate::player::arm_resume_pend(&mut rig.ps);
        assert!(rig.ps.readout().paused() && rig.ps.readout().seek_preroll() && rig.ps.readout().resume_pend(),
            "premise: all three are standing");
        crate::player::assert_transport_copies_agree(&rig.ps);
        crate::player::stop_bufferfeed(&mut rig.ps, &mut rig.pa);
        let r = rig.ps.readout();
        assert!(!r.paused() && !r.seek_preroll() && !r.resume_pend(), "the readout's three are gone");
        assert!(!FEED_TX.paused.load(Acquire) && !FEED_TX.seek_preroll_active(), "and the feed's two");
        crate::player::assert_transport_copies_agree(&rig.ps);
    }

    // ---- the paused-seek lifecycle across EVERY route a seek can take, and across a reload ----

    pub(super) fn preroll_at_reload() -> Vec<bool> {
        super::super::engine::RELOAD_PREROLL_LOG.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    impl Rig {
        /// What the real `reload_at` / `reload_transcode` do once their route accepts (this rig has
        /// no route, so the real call returns `NoRoute` after logging): `teardown(for_reload)`
        /// resets the session and keeps the transport's Pause and preroll, the fresh Engine arms its
        /// Initial hold from those same two inputs (`engine::ClockStartGuard::arm`), and the new
        /// session waits for its first keyframe at `target`.
        pub(super) fn complete_reload(&mut self, target: i64) {
            FEED_TX.reset_for_reload();
            crate::player::reset_session_for_reload_for_test();
            self.ps.step_readout(crate::player::readout::ReadoutEvent::SessionReset);
            FEED_SHARED.reset_hls_clock_for_test();
            assert!(FEED_SHARED.arm_initial_clock_hold(FEED_TX.paused.load(Acquire), FEED_TX.seek_preroll_active()));
            crate::player::arm_open(&mut self.pa, target);
            let epoch = FEED_SHARED.begin_native_session().unwrap_or(0);
            let mut fresh = super::super::engine::streaming_engine_for_test(epoch);
            fresh.stage = Stage::Playing;
            fresh.rebase_pending = true;
            self.pa.install(fresh);
            FEED_SHARED.duration_ns.store(7_200_000_000_000, Relaxed);
            FEED_SHARED.file_size.store(50_000_000_000, Relaxed);
            FEED_SHARED.seeking.store(true, Relaxed);
        }

        /// Frame until `reloads()` has an entry (the dispatcher ran `reload_at`).
        fn frame_until_reload(&mut self) {
            for _ in 0..20 {
                if !reloads().is_empty() {
                    return;
                }
                self.frame();
            }
            panic!("the pump never dispatched a reload");
        }

        /// The fresh session's keyframe lands and its first frame is presented; returns the number of
        /// physical Plays the prime issued meanwhile.
        fn land_and_present(&mut self, ns: i64) -> u64 {
            let plays = ffi_host::play_calls_for_test();
            self.land_keyframe_aus(ns, 90);
            for _ in 0..8 {
                self.frame();
            }
            let issued = ffi_host::play_calls_for_test() - plays;
            FEED_SHARED.frames.store(1, Relaxed);
            self.frame();
            issued
        }
    }

    fn assert_ended_paused() {
        assert!(!FEED_TX.seek_preroll_active(), "the preroll closed on the landing");
        assert!(FEED_TX.paused.load(Acquire), "the viewer is still paused");
        assert!(!ffi_host::clock_state_for_test().1, "the physical clock is held again: the film is not playing");
    }

    #[test]
    fn a_paused_scrub_in_reload_mode_holds_the_preroll_across_the_reload_and_ends_paused() {
        let _serial = plx_base::testlock::serial();
        let mut rig = Rig::new(None);
        plx_play_feed::seek::set_seek_mode(SeekMode::Reload);
        rig.viewer_pause();
        crate::player::request_seek(&mut rig.ps, &mut rig.pa, TARGET_NS);
        rig.frame_until_reload();
        assert_eq!(reloads(), vec![TARGET_NS]);
        assert_eq!(preroll_at_reload(), vec![true], "the fresh Load is dispatched with the feed gate open");
        rig.complete_reload(TARGET_NS);
        assert!(FEED_TX.feed_allowed(), "the new session may feed its keyframe");
        let plays = rig.land_and_present(TARGET_NS);
        assert!(plays >= 1, "the prime Play fired through the reopened gate before the frame was presented");
        assert_eq!(reloads(), vec![TARGET_NS], "no second reload");
        assert_ended_paused();
    }

    #[test]
    fn two_consecutive_paused_reload_mode_seeks_each_open_the_preroll_and_end_paused() {
        let _serial = plx_base::testlock::serial();
        let mut rig = Rig::new(None);
        plx_play_feed::seek::set_seek_mode(SeekMode::Reload);
        rig.viewer_pause();
        for target in [TARGET_NS, NEXT_NS] {
            super::super::engine::RELOAD_AT_LOG.lock().unwrap_or_else(|e| e.into_inner()).clear();
            super::super::engine::RELOAD_PREROLL_LOG.lock().unwrap_or_else(|e| e.into_inner()).clear();
            FEED_SHARED.frames.store(0, Relaxed);
            crate::player::request_seek(&mut rig.ps, &mut rig.pa, target);
            rig.frame_until_reload();
            assert_eq!(reloads(), vec![target]);
            assert_eq!(preroll_at_reload(), vec![true], "the reload at {target} is dispatched with the gate open");
            rig.complete_reload(target);
            assert!(rig.land_and_present(target) >= 1, "the prime Play fired through the reopened gate");
            assert_eq!(reloads(), vec![target], "no second reload");
            assert_ended_paused();
        }
    }

    #[test]
    fn a_paused_transcode_seek_landing_reloads_with_the_preroll_open() {
        let _serial = plx_base::testlock::serial();
        let mut rig = Rig::new(None);
        rig.viewer_pause();
        // The flight's landing as the drain runs it. The rig has no route, so `reload_transcode`
        // itself returns after its entry probe; what is graded is what the landing dispatches WITH
        // (the state after the reload is the one the Reload-mode test above grades end to end).
        super::seek_landing_tail(
            &mut rig.ps,
            &mut rig.pa,
            7,
            TARGET_NS,
            crate::route::SeekVerdict::Install(String::from("http://pms.invalid/start.mkv")),
        );
        assert_eq!(preroll_at_reload(), vec![true], "the viewer's Pause must not shut the fresh stream's gate");
    }

    #[test]
    fn a_transcode_seek_landing_the_claim_hold_paused_does_not_open_a_preroll() {
        let _serial = plx_base::testlock::serial();
        let mut rig = Rig::new(None);
        // The hold (not the viewer) paused a PLAYING stream and owes the Play back after the reload.
        assert!(crate::player::pause(&mut rig.ps, &mut rig.pa));
        super::super::claim_hold::hold_for_test(7, true);
        super::seek_landing_tail(
            &mut rig.ps,
            &mut rig.pa,
            7,
            TARGET_NS,
            crate::route::SeekVerdict::Install(String::from("http://pms.invalid/start.mkv")),
        );
        assert_eq!(preroll_at_reload(), vec![false], "a hold's pause is given back, never re-asserted");
        super::super::claim_hold::clear();
    }

    // ---- a fresh session under a viewer Pause is primed a first frame whatever reloaded it ----

    impl Rig {
        /// What every non-seek reload does once its route accepts (`reload_transcode`,
        /// `switch_audio_native`, the adaptive and Original landings): the session resets keeping
        /// the transport's Pause, and the fresh Engine arms its Initial hold from it. No seek is
        /// armed and no preroll is opened by the caller.
        fn complete_plain_reload(&mut self) {
            FEED_TX.reset_for_reload();
            crate::player::reset_session_for_reload_for_test();
            FEED_SHARED.reset_hls_clock_for_test();
            assert!(FEED_SHARED.arm_initial_clock_hold(FEED_TX.paused.load(Acquire), FEED_TX.seek_preroll_active()));
            let epoch = FEED_SHARED.begin_native_session().unwrap_or(0);
            let mut fresh = super::super::engine::streaming_engine_for_test(epoch);
            fresh.stage = Stage::Playing;
            self.pa.install(fresh);
            FEED_SHARED.duration_ns.store(7_200_000_000_000, Relaxed);
            FEED_SHARED.file_size.store(50_000_000_000, Relaxed);
            FEED_SHARED.playpos_ns.store(TARGET_NS, Relaxed);
        }
    }

    #[test]
    fn a_paused_non_seek_reload_primes_a_first_frame_and_ends_paused() {
        let _serial = plx_base::testlock::serial();
        let mut rig = Rig::new(None);
        FEED_SHARED.frames.store(1, Relaxed);
        rig.viewer_pause();
        // retranscode_tail / native_audio_tail / adaptive_tail: no seek, the preroll not opened.
        assert!(!FEED_TX.seek_preroll_active());
        rig.complete_plain_reload();
        assert_eq!(FEED_SHARED.frames.load(Relaxed), 0, "the reload zeroed the picture");
        rig.frame();
        assert!(FEED_TX.feed_allowed(), "the fresh session must be allowed to feed a keyframe");
        let plays = rig.land_and_present(TARGET_NS);
        assert!(plays >= 1, "the prime Play fired through the opened gate before the frame was presented");
        assert_ended_paused();
    }

    /// THE reported path: a direct-play (native) session, the viewer paused, then another audio
    /// track picked. `native_audio_tail` -> `switch_audio_native` -> `reload_at` arms a demuxer seek
    /// at the playhead but is NOT a seek the pump dispatched (`FEED_SHARED.seeking` stays false), so
    /// neither seek preroll covers it.
    #[test]
    fn a_paused_native_audio_track_switch_primes_a_first_frame_and_ends_paused() {
        let _serial = plx_base::testlock::serial();
        let mut rig = Rig::new(None);
        FEED_SHARED.frames.store(1, Relaxed);
        rig.viewer_pause();
        let pos = FEED_SHARED.playpos_ns.load(Relaxed);
        // The real entry the tail calls; this rig has no route, so it logs and returns NoRoute.
        let _ = super::super::engine::switch_audio_native(&mut rig.ps, &mut rig.pa, 2, pos);
        assert_eq!(reloads(), vec![pos], "the audio switch reloaded at the playhead");
        assert_eq!(FEED_SHARED.desired_audio_idx.load(Relaxed), 2, "the pick reached the feed's demuxer copy at once");
        assert_eq!(rig.ps.readout().audio_choice(), 2, "and the session's own record");
        // What `reload_at` leaves once its route accepts: the session reset keeping the Pause, a
        // demuxer seek armed at the playhead, a fresh Engine awaiting its keyframe's rebase.
        rig.complete_reload(pos);
        FEED_SHARED.seeking.store(false, Relaxed);
        assert_eq!(FEED_SHARED.frames.load(Relaxed), 0);
        rig.frame();
        assert!(FEED_TX.feed_allowed(), "the new audio session must be allowed to feed its first keyframe");
        let plays = rig.land_and_present(pos);
        assert!(plays >= 1, "the prime Play fired through the opened gate before the frame was presented");
        assert_eq!(reloads(), vec![pos], "no second reload");
        assert_ended_paused();
    }

    /// The prime's Play already fired (`prime_play` cleared, the clock Running) and the viewer then
    /// pressed Pause before the first position update: `frames == 0` and the clock is
    /// `Held(User)`. No prime is owed, so converting that into `Held(Seek)` with `user_held` false
    /// would strand the film: nothing plays, and Play has no user hold to take. The pump opens no
    /// preroll, and a later Play still plays.
    #[test]
    fn a_pause_after_the_prime_play_but_before_the_first_frame_opens_no_preroll_and_play_resumes() {
        let _serial = plx_base::testlock::serial();
        let mut rig = Rig::new(None);
        FEED_SHARED.frames.store(0, Relaxed);
        FEED_SHARED.reset_hls_clock_for_test();
        assert!(FEED_SHARED.arm_initial_clock_hold(false, false));
        rig.eng().prime_play = true;
        let plays = ffi_host::play_calls_for_test();
        rig.land_keyframe_aus(60_000_000_000, 90);
        for _ in 0..8 {
            rig.frame();
        }
        assert!(ffi_host::play_calls_for_test() > plays, "the prime Play fired");
        assert!(!rig.eng().prime_play, "the prime is no longer owed");
        assert_eq!(FEED_SHARED.frames.load(Relaxed), 0, "no position update yet");
        assert!(crate::player::pause(&mut rig.ps, &mut rig.pa), "viewer Pause accepted");
        for _ in 0..4 {
            rig.frame();
        }
        assert!(!FEED_TX.seek_preroll_active(), "a preroll opened with no prime owed: nothing would ever Play");
        let plays = ffi_host::play_calls_for_test();
        assert!(crate::player::resume(&mut rig.ps, &mut rig.pa), "Play after the Pause is accepted");
        for _ in 0..4 {
            rig.frame();
        }
        assert!(!FEED_TX.paused.load(Acquire), "the player is unpaused");
        assert!(ffi_host::play_calls_for_test() > plays, "Play issued a native Play");
        assert!(ffi_host::clock_state_for_test().1, "the physical clock runs again");
    }

    /// A reload that lands BEHIND the old playhead while paused (the viewer scrubbed back more than
    /// 15 s and switched audio before the scrub dispatched): the preroll the pump opens for the
    /// pictureless session must close after its one frame, whatever `playpos_ns` still says.
    #[test]
    fn a_paused_reload_landing_far_behind_the_old_playhead_ends_paused_after_one_frame() {
        let _serial = plx_base::testlock::serial();
        let mut rig = Rig::new(None);
        FEED_SHARED.frames.store(1, Relaxed);
        rig.viewer_pause();
        let behind = 20_000_000_000; // the old playhead is 60 s: more than 15 s ahead of the landing
        rig.complete_plain_reload();
        FEED_SHARED.playpos_ns.store(60_000_000_000, Relaxed);
        // A transcode reload (`reload_transcode`) starts the encoder at the target and arms no demuxer
        // seek, so `playpos_ns` keeps the OLD playhead until the new session's first frame reports.
        assert_eq!(FEED_SHARED.playpos_ns.load(Relaxed), 60_000_000_000);
        rig.frame();
        assert!(FEED_TX.feed_allowed(), "the pictureless session is primed a frame");
        let plays = ffi_host::play_calls_for_test();
        rig.land_keyframe_aus(behind, 90);
        for _ in 0..8 {
            rig.frame();
        }
        assert!(ffi_host::play_calls_for_test() > plays, "the prime Play fired");
        // The first presented frame reports the landing's position (`pump` writes `playpos_ns`).
        FEED_SHARED.playpos_ns.store(behind, Relaxed);
        FEED_SHARED.frames.store(1, Relaxed);
        for _ in 0..3 {
            rig.frame();
        }
        assert_ended_paused();
    }

    /// The owner's framing: "Buffering…" while the player does NO work is a lie. A viewer-paused
    /// session with no picture, the feed gate shut, and nothing in flight (no claim flight, no seek,
    /// no preroll, the Engine loaded) publishes a state the HUD draws as the normal paused
    /// transport, not the centred spinner. (The clock is left un-held here so the preroll rule
    /// cannot open: this grades the readout rule on its own.)
    #[test]
    fn a_paused_idle_session_with_no_picture_does_not_read_buffering() {
        let _serial = plx_base::testlock::serial();
        let mut rig = Rig::new(None);
        FEED_SHARED.reset_hls_clock_for_test(); // Running: not a clock the preroll rule can take over
        crate::player::commit_paused(&mut rig.ps, &mut rig.pa, true);
        assert_eq!(FEED_SHARED.frames.load(Relaxed), 0);
        rig.frame();
        assert!(!FEED_TX.seek_preroll_active(), "sanity: nothing is working, no preroll");
        assert!(!FEED_TX.feed_allowed(), "sanity: the feed gate is shut");
        let st = crate::player::state(&rig.ps);
        assert_eq!(
            st,
            crate::player::PlaybackState::Paused,
            "neither a spinner (Buffering) nor a lie (Playing) over an idle paused player"
        );
        assert!(!st.is_busy(), "the HUD's busy surface (`Busy::None`) follows `is_busy`");
        assert!(!crate::player::is_playing(&rig.ps), "`is_playing` means frames are presented");
    }

    /// The telemetry follows the published state: a paused, idle session with no picture is not a
    /// playback that started (`PlaybackStarted`).
    #[test]
    fn a_paused_idle_session_with_no_picture_reports_no_playback_started() {
        let _serial = plx_base::testlock::serial();
        let mut rig = Rig::new(None);
        super::super::report::reset_latches_for_test();
        FEED_SHARED.reset_hls_clock_for_test();
        crate::player::commit_paused(&mut rig.ps, &mut rig.pa, true);
        rig.frame();
        assert!(!FEED_TX.feed_allowed(), "sanity: the idle paused state");
        super::super::report::tick(&rig.ps);
        let started = super::super::report::error_context_for_test(&rig.ps).started;
        super::super::report::reset_latches_for_test();
        assert!(!started, "PlaybackStarted fired for a session that never presented a frame");
    }

    /// ...and the same session with real work pending still reads Buffering: a seek preroll open.
    #[test]
    fn a_paused_session_with_a_preroll_open_still_reads_buffering() {
        let _serial = plx_base::testlock::serial();
        let mut rig = Rig::new(None);
        crate::player::commit_paused(&mut rig.ps, &mut rig.pa, true);
        crate::player::begin_paused_seek(&mut rig.ps, &mut rig.pa);
        assert!(FEED_TX.seek_preroll_active(), "the preroll opened under the Pause");
        rig.frame();
        assert_eq!(crate::player::state(&rig.ps), crate::player::PlaybackState::Buffering);
    }

    #[test]
    fn a_pause_pressed_on_an_initial_hold_before_any_frame_primes_one_and_ends_paused() {
        let _serial = plx_base::testlock::serial();
        let mut rig = Rig::new(None);
        FEED_SHARED.reset_hls_clock_for_test();
        assert!(FEED_SHARED.arm_initial_clock_hold(false, false), "a fresh Engine is Held(Initial)");
        assert!(crate::player::pause(&mut rig.ps, &mut rig.pa), "Pause on the held clock is accepted as already held");
        assert_eq!(FEED_SHARED.frames.load(Relaxed), 0);
        rig.frame();
        assert!(FEED_TX.feed_allowed(), "a Pause before the first frame must not starve the first frame");
        let plays = rig.land_and_present(60_000_000_000);
        assert!(plays >= 1, "the prime Play fired through the opened gate");
        assert_ended_paused();
    }

    #[test]
    fn a_common_startup_paused_reload_primes_without_a_viewer_play_command() {
        let _serial = plx_base::testlock::serial();
        let mut rig = Rig::new(None);
        rig.eng().startup_controlled = true;
        rig.eng().startup_play_commanded = false;
        rig.eng().startup_user_held = true;
        FEED_SHARED.reset_hls_clock_for_test();
        assert!(FEED_SHARED.arm_initial_clock_hold(false, false));
        assert!(crate::player::pause(&mut rig.ps, &mut rig.pa));
        rig.frame();
        assert!(FEED_TX.seek_preroll_active(), "the paused reload owns a bounded first-picture preroll");
        let plays = rig.land_and_present(60_000_000_000);
        assert!(plays >= 1, "the internal preroll must present a frame without a viewer Play");
        assert!(!rig.eng().startup_play_commanded, "preroll must not invent a viewer Play command");
        assert_ended_paused();
    }

    #[test]
    fn a_common_startup_without_a_paused_preroll_still_waits_for_the_play_command() {
        let _serial = plx_base::testlock::serial();
        let mut rig = Rig::new(None);
        rig.eng().startup_controlled = true;
        rig.eng().startup_play_commanded = false;
        FEED_SHARED.reset_hls_clock_for_test();
        assert!(FEED_SHARED.arm_initial_clock_hold(false, false));
        let plays = ffi_host::play_calls_for_test();
        rig.land_keyframe_aus(60_000_000_000, 90);
        for _ in 0..8 {
            rig.frame();
        }
        assert!(!FEED_TX.seek_preroll_active());
        assert_eq!(ffi_host::play_calls_for_test(), plays, "ordinary startup still waits for Play");
        assert!(rig.eng().prime_play);
    }

    #[test]
    fn a_paused_scrub_whose_native_seek_is_refused_reloads_and_ends_paused() {
        let _serial = plx_base::testlock::serial();
        let mut rig = Rig::new(Some(0));
        rig.viewer_pause();
        rig.arm(TARGET_NS);
        rig.land_keyframe(TARGET_NS);
        rig.frame(); // the feed refuses the Seek and re-posts the target for the reload
        rig.now += 5_000;
        rig.frame_until_reload();
        assert_eq!(reloads(), vec![TARGET_NS]);
        assert_eq!(preroll_at_reload(), vec![true]);
        // The replacement Engine is a different owner than the one that armed the seek.
        rig.complete_reload(TARGET_NS);
        let plays = rig.land_and_present(TARGET_NS);
        assert!(plays >= 1, "the prime Play fired through the reopened gate");
        assert_ended_paused();
    }

    #[test]
    fn a_refused_repause_is_retried_until_it_lands() {
        let _serial = plx_base::testlock::serial();
        let mut rig = Rig::new(None);
        rig.viewer_pause();
        rig.arm(TARGET_NS);
        let plays = ffi_host::play_calls_for_test();
        rig.land_keyframe_aus(TARGET_NS, 90);
        for _ in 0..8 {
            rig.frame();
        }
        assert!(ffi_host::play_calls_for_test() > plays, "the prime Play fired through the opened gate");
        ffi_host::force_pause_result_for_test(Some(0)); // Starfish refuses the re-pause
        struct Unforce;
        impl Drop for Unforce {
            fn drop(&mut self) {
                ffi_host::force_pause_result_for_test(None);
            }
        }
        let _unforce = Unforce;
        FEED_SHARED.frames.store(1, Relaxed);
        for _ in 0..3 {
            rig.frame();
        }
        assert!(FEED_TX.seek_preroll_active(), "a refused re-pause leaves the preroll open and the pump retrying");
        ffi_host::force_pause_result_for_test(None);
        rig.frame();
        assert_ended_paused();
    }

    // ---- the paused preroll's Play window is silent ----

    /// A paused seek, driven to the point the prime has Played through the opened gate.
    fn paused_seek_playing_its_preroll(rig: &mut Rig) {
        rig.viewer_pause();
        ffi_host::reset_volume_probe_for_test();
        rig.arm(TARGET_NS);
        rig.land_keyframe_aus(TARGET_NS, 90);
        for _ in 0..8 {
            rig.frame();
        }
    }

    /// The preroll's Play window is audible, so the sink is muted BEFORE that Play and the volume
    /// is given back only AFTER the accepted re-pause: the order is the whole fix.
    #[test]
    fn a_paused_preroll_mutes_before_its_play_and_restores_after_its_repause() {
        let _serial = plx_base::testlock::serial();
        let mut rig = Rig::new(None);
        paused_seek_playing_its_preroll(&mut rig);
        assert_eq!(ffi_host::host_volume_for_test(), 0, "sanity: muted while the preroll plays");
        FEED_SHARED.frames.store(1, Relaxed);
        for _ in 0..3 {
            rig.frame();
        }
        assert!(!FEED_TX.seek_preroll_active(), "the preroll closed on the landing");
        let verbs = ffi_host::verb_order_for_test();
        let at = |v: &str| verbs.iter().position(|x| x == v).unwrap_or_else(|| panic!("no {v} in {verbs:?}"));
        assert!(at("volume:0") < at("play"), "muted before the Play: {verbs:?}");
        assert!(at("play") < at("pause"), "{verbs:?}");
        // No getVolume is bound (it returns an int on 4.5, not the header's string), so the restore
        // is the pipeline default by construction, not a value read back.
        let default = plx_platform::tv::sink::PIPELINE_DEFAULT_VOLUME;
        assert_eq!(default, 100, "libpf CustomPipeline::getVolume's default");
        assert!(at("pause") < at(&format!("volume:{default}")), "restored after the re-pause: {verbs:?}");
        assert_eq!(ffi_host::host_volume_for_test(), default);
        assert!(!FEED_TX.preroll_muted());
    }

    /// A normal (unpaused) seek prime is not a preroll: it never touches the volume.
    #[test]
    fn an_unpaused_seek_prime_never_sets_the_volume() {
        let _serial = plx_base::testlock::serial();
        let mut rig = Rig::new(None);
        ffi_host::reset_volume_probe_for_test();
        rig.arm(TARGET_NS);
        let plays = ffi_host::play_calls_for_test();
        rig.land_keyframe_aus(TARGET_NS, 90);
        for _ in 0..8 {
            rig.frame();
        }
        assert!(ffi_host::play_calls_for_test() > plays, "sanity: the prime Played");
        let verbs = ffi_host::verb_order_for_test();
        assert!(!verbs.iter().any(|v| v.starts_with("volume:")), "an unpaused start set the volume: {verbs:?}");
    }

    /// The viewer presses Play while the preroll's Play window is open: the volume is back no later
    /// than that Resume.
    #[test]
    fn resume_during_an_open_preroll_restores_the_volume() {
        let _serial = plx_base::testlock::serial();
        let mut rig = Rig::new(None);
        paused_seek_playing_its_preroll(&mut rig);
        assert!(FEED_TX.seek_preroll_active() && FEED_TX.preroll_muted(), "sanity: muted preroll open");
        assert!(crate::player::resume(&mut rig.ps, &mut rig.pa), "viewer Resume accepted");
        assert_eq!(ffi_host::host_volume_for_test(), 100, "Resume left the sink muted");
        assert!(!FEED_TX.preroll_muted());
    }

    /// A preroll closed by anything but the accepted re-pause (an abandoned seek) still gives the
    /// volume back, on the pump's next tick.
    #[test]
    fn an_abandoned_preroll_restores_the_volume_on_the_next_tick() {
        let _serial = plx_base::testlock::serial();
        let mut rig = Rig::new(None);
        paused_seek_playing_its_preroll(&mut rig);
        assert_eq!(ffi_host::host_volume_for_test(), 0, "sanity: muted");
        crate::player::abandon_seek(&mut rig.ps, &mut rig.pa);
        assert!(!FEED_TX.seek_preroll_active());
        rig.frame();
        assert_eq!(ffi_host::host_volume_for_test(), 100, "an abandoned preroll left the sink muted");
        assert!(!FEED_TX.preroll_muted());
    }

    /// A stop unloads the pipeline the mute belonged to, so the next playback starts unmuted with
    /// no sink call on the dead one.
    #[test]
    fn a_stop_forgets_the_mute_with_its_pipeline() {
        let _serial = plx_base::testlock::serial();
        let mut rig = Rig::new(None);
        paused_seek_playing_its_preroll(&mut rig);
        assert!(FEED_TX.preroll_muted(), "sanity: muted");
        let sets = ffi_host::verb_order_for_test().len();
        FEED_TX.reset();
        assert!(!FEED_TX.preroll_muted());
        assert_eq!(ffi_host::verb_order_for_test().len(), sets, "a reset touched the dead pipeline");
    }

    #[test]
    fn a_pause_pressed_mid_flight_of_a_playing_seek_still_lands_and_ends_paused() {
        let _serial = plx_base::testlock::serial();
        let mut rig = Rig::new(None);
        rig.arm(TARGET_NS); // playing: no preroll, nothing to hold
        assert!(!FEED_TX.seek_preroll_active());
        assert!(crate::player::pause(&mut rig.ps, &mut rig.pa), "the viewer pauses before the keyframe landed");
        rig.frame();
        assert!(FEED_TX.feed_allowed(), "the keyframe must still be able to land");
        let plays = ffi_host::play_calls_for_test();
        rig.land_keyframe_aus(TARGET_NS, 90);
        for _ in 0..8 {
            rig.frame();
        }
        assert!(ffi_host::play_calls_for_test() > plays, "the prime Play fired through the opened gate");
        assert!(!rig.eng().flushed && !rig.eng().rebase_pending, "the keyframe reached feed_stream's rebase");
        rig.now += 5_000;
        for _ in 0..3 {
            rig.frame();
        }
        assert!(reloads().is_empty(), "no stuck-watchdog reload");
        FEED_SHARED.frames.store(1, Relaxed);
        rig.frame();
        assert_ended_paused();
        assert!(!FEED_SHARED.seeking.load(Relaxed), "no endless spinner");
    }

    // ---- the ACB (webOS 4) video plane binds through setMediaVideoData + acb_start; a paused
    // preroll's picture is only on the panel once that bind has completed ----

    /// ACB in use, `setMediaVideoData` accepted, the sourceInfo envelope captured; restores the
    /// process-wide plane owner (no handle by default on the host) when the test ends.
    struct AcbGuard;
    impl AcbGuard {
        fn arm() -> AcbGuard {
            crate::player::plane().reset_for_test();
            crate::player::plane().boot(plx_platform::tv::plane::PlaneFacts { handle: true, player_type: 10 });
            ffi_host::reset_acb_for_test(true);
            AcbGuard
        }
    }
    impl Drop for AcbGuard {
        fn drop(&mut self) {
            crate::player::plane().reset_for_test();
            ffi_host::reset_acb_for_test(false);
        }
    }

    impl Rig {
        /// A fresh ACB session that has not bound yet (what a reload installs).
        fn unbound_acb_session(&mut self) {
            // `Rig::new` reset the session, which cleared what the pipeline's callbacks captured.
            *FEED_SHARED.media_id.lock().unwrap() = Some(std::ffi::CString::new("_media_id_").unwrap());
            *FEED_SHARED.source_info.lock().unwrap() = Some(b"{\"context\":\"x\",\"video\":{}}\0".to_vec());
            let mut e = self.eng();
            e.video_info_sent = false;
            e.prime_play = true;
            e.bound_at_frames = None;
            e.stage = Stage::Playing;
        }
    }

    /// THE reported bug: the paused reload preroll re-paused at the first position tick, below
    /// `Stage::Streaming`, so `setMediaVideoData` + `acb_start` never ran and the panel stayed
    /// black until Resume.
    #[test]
    fn a_paused_acb_preroll_does_not_re_pause_before_the_plane_is_bound() {
        let _serial = plx_base::testlock::serial();
        let _acb = AcbGuard::arm();
        let mut rig = Rig::new(None);
        rig.unbound_acb_session();
        rig.viewer_pause();
        FEED_SHARED.frames.store(0, Relaxed);
        rig.frame();
        rig.frame();
        assert!(FEED_TX.seek_preroll_active(), "the reload preroll opened");
        ffi_host::reset_acb_for_test(true);
        let mut closed_at = None;
        for tick in 1..=6 {
            FEED_SHARED.frames.store(tick, Relaxed);
            rig.frame();
            rig.frame();
            if !FEED_TX.seek_preroll_active() {
                closed_at = Some(tick);
                break;
            }
        }
        let closed_at = closed_at.expect("the preroll closed");
        assert!(closed_at > 1, "re-paused on the first tick, before the plane could bind (tick {closed_at})");
        assert!(rig.eng().stage == Stage::Streaming, "re-paused below Streaming: the plane is not bound");
        let (video_data, start, pause, _) = ffi_host::acb_calls_for_test();
        assert_eq!((video_data, start), (1, 1), "the bind ran exactly once before the re-pause");
        assert_eq!(pause, 1, "the re-pause mirrors ACB PAUSED now that the plane is bound");
        assert!(FEED_TX.paused.load(Acquire), "the film ends paused");
    }

    /// The Play window of the preroll is as short as the bind allows: during an open preroll the
    /// decoder having produced its first tick is enough (outside one the gate stays at two).
    #[test]
    fn an_open_acb_preroll_binds_at_the_first_tick() {
        let _serial = plx_base::testlock::serial();
        let _acb = AcbGuard::arm();
        let mut rig = Rig::new(None);
        rig.unbound_acb_session();
        rig.viewer_pause();
        FEED_SHARED.frames.store(0, Relaxed);
        rig.frame();
        rig.frame();
        assert!(FEED_TX.seek_preroll_active());
        ffi_host::reset_acb_for_test(true);
        FEED_SHARED.frames.store(1, Relaxed);
        rig.frame();
        let (video_data, start, pause, _) = ffi_host::acb_calls_for_test();
        assert_eq!((video_data, start), (1, 1), "the plane bound on the preroll's first tick");
        assert_eq!(pause, 0, "the pipeline is Playing for the preroll, so ACB stays PLAYING");
        assert!(rig.eng().stage == Stage::Streaming);
    }

    #[test]
    fn a_normal_acb_start_still_waits_for_the_second_tick() {
        let _serial = plx_base::testlock::serial();
        let _acb = AcbGuard::arm();
        let mut rig = Rig::new(None);
        rig.unbound_acb_session();
        FEED_SHARED.frames.store(1, Relaxed);
        rig.frame();
        rig.frame();
        assert_eq!(ffi_host::acb_calls_for_test().1, 0, "one tick is not enough outside a preroll");
        FEED_SHARED.frames.store(2, Relaxed);
        rig.frame();
        assert_eq!(ffi_host::acb_calls_for_test().1, 1);
        assert_eq!(ffi_host::acb_calls_for_test().2, 0, "playing: ACB is left PLAYING");
    }

    /// A late tick binds the plane after the pipeline was already re-paused: ACB must not be left
    /// PLAYING under a paused pipeline.
    #[test]
    fn an_acb_start_under_a_paused_pipeline_is_followed_by_the_acb_pause() {
        let _serial = plx_base::testlock::serial();
        let _acb = AcbGuard::arm();
        let mut rig = Rig::new(None);
        rig.unbound_acb_session();
        rig.eng().prime_play = false;
        rig.eng().stage = Stage::Bound;
        FEED_SHARED.frames.store(2, Relaxed);
        rig.viewer_pause();
        assert!(!FEED_TX.seek_preroll_active(), "no preroll: the pipeline itself is paused");
        ffi_host::reset_acb_for_test(true);
        rig.frame();
        let (_, start, pause, _) = ffi_host::acb_calls_for_test();
        assert_eq!(start, 1, "the bind ran");
        assert_eq!(pause, 1, "ACB was left PLAYING under a paused pipeline");
    }
}

#[cfg(all(test, feature = "hostsim"))]
#[path = "dv_start_pump_tests.rs"]
mod dv_start_pump_tests;

/// **A film whose audio outlasts its video ends on the host sink too (#865).** The pipeline stops
/// its clock at the last video frame, so the playhead never comes within [`EOS_TAIL_NS`](crate::player::readout::EOS_TAIL_NS) of the
/// container's duration (the longest track's) and only the pipeline's own end-of-stream can end
/// the film. These drive the REAL feed lanes and the REAL `pump` against the clock sink and grade
/// `player::ended()`. Nothing here sets `Shared::pipeline_eos` or `ended`: what is graded is the
/// whole chain, the sink, its callback, the flag and the pump.
#[cfg(all(test, feature = "hostsim"))]
mod end_of_film_tests {
    use std::sync::atomic::Ordering::Relaxed;
    use super::*;
    use plx_play_feed::shared::FEED_TX;
    use plx_play_feed::ffi_host;
    use crate::player::readout::EOS_TAIL_NS;
    use std::os::raw::c_int;

    const S: i64 = 1_000_000_000;
    const V_STEP_NS: i64 = 41_666_666; // 24 fps
    const A_STEP_NS: i64 = 21_333_333; // AAC: 1024 samples at 48 kHz
    const VIDEO_END_NS: i64 = 20 * S;
    const AUDIO_END_NS: i64 = 23 * S;
    /// The pipeline's position-callback period (`TICK_MS` in the sink), milliseconds.
    const TICK_MS: i64 = 200;
    /// Steps a run may take: 23 s at one tick per step is 115.
    const STEP_BUDGET: usize = 400;

    struct Rig {
        ps: crate::route::PlaybackSession,
        pa: super::super::adapter::PlayerAdapter,
        now: u32,
    }

    impl Rig {
        /// A direct-play session on a REALLY loaded host object (so `ACTIVE_EPOCH` is set and the
        /// callback gate is open: otherwise the sink reports nothing and a red run would be a dead
        /// instrument, not the defect), holding 20 s of video and 23 s of audio, primed to play.
        fn new() -> Rig {
            crate::player::reset_session_for_test();
            FEED_TX.reset();
            super::super::claim_hold::clear();
            ffi_host::reset_native_lifecycle_for_test();
            ffi_host::force_clocksink_for_test(true);
            let mut pa = super::super::adapter::PlayerAdapter::new_probed(unsafe { MainThread::assume() });
            let epoch = FEED_SHARED.begin_native_session().expect("native session");
            assert_eq!(
                unsafe { crate::player::test_physical::sink().load(c"{}".as_ptr(), epoch) },
                1,
                "PRECONDITION FAILED, not the defect: the host Load did not take"
            );
            assert!(FEED_SHARED.mark_native_load_returned(epoch, None).is_some());
            let mut eng = super::super::engine::streaming_engine_for_test(epoch);
            eng.load_th = None;
            assert!(eng.stage >= Stage::Playing, "PRECONDITION FAILED, not the defect: the engine is not past its Load");
            let au = [0u8; 64];
            let qv = &mut **eng.aq_video.as_mut().unwrap() as *mut plx_play_feed::aq::AuQueue;
            for i in 0..VIDEO_END_NS / V_STEP_NS {
                plx_play_feed::aq::aq_push(qv, au.as_ptr(), au.len() as c_int, i * V_STEP_NS, c_int::from(i == 0), 1);
            }
            let qa = &mut **eng.aq_audio.as_mut().unwrap() as *mut plx_play_feed::aq::AuQueue;
            for i in 0..AUDIO_END_NS / A_STEP_NS {
                plx_play_feed::aq::aq_push(qa, au.as_ptr(), au.len() as c_int, i * A_STEP_NS, 1, 2);
            }
            pa.install(eng);
            FEED_SHARED.duration_ns.store(AUDIO_END_NS, Relaxed);
            // The pump's `present` addresses the attempt the session's view began for: the readout is
            // what `player::state` and `player::ended` read, so the rig owes it that attempt.
            let mut ps = crate::route::PlaybackSession::IDLE;
            ps.begin_attempt_view(crate::route::RouteStartAttempt::fixture(), super::super::attempt_view::Backend::Feed);
            Rig { ps, pa, now: 1_000 }
        }

        fn eng(&mut self) -> std::cell::RefMut<'_, Engine> {
            self.pa.engine().expect("engine")
        }

        /// The demuxer reached the end of the file: both lanes are complete.
        fn producer_done(&mut self) {
            let mut guard = self.eng();
            let eng = &mut *guard;
            for q in [eng.aq_video.as_mut().unwrap(), eng.aq_audio.as_mut().unwrap()] {
                plx_play_feed::aq::aq_set_eof(&mut **q as *mut plx_play_feed::aq::AuQueue);
            }
        }

        /// One pipeline period: the clock runs 200 ms, the sink takes its tick, the frame runs.
        fn step(&mut self) {
            ffi_host::advance_clock_for_test(TICK_MS);
            ffi_host::tick_for_test();
            let _frame = plx_base::task::FrameScope::enter();
            self.now += TICK_MS as u32;
            pump(&mut self.ps, &mut self.pa, self.now);
        }

        /// Step until `done`, within [`STEP_BUDGET`].
        fn run_until(&mut self, done: impl Fn(&mut Rig) -> bool) -> bool {
            for _ in 0..STEP_BUDGET {
                self.step();
                if done(self) {
                    return true;
                }
            }
            false
        }

        /// What a failure message needs to explain itself.
        fn facts(&mut self) -> String {
            let (clock_ns, running) = ffi_host::clock_state_for_test();
            let ended = crate::player::feed_ended(&self.ps);
            let streaming = self.eng().stage >= Stage::Streaming;
            let pending = self.eng().pending_video.is_some();
            let seek_pending = self.ps.readout().pending_seek().unwrap_or(-1);
            format!(
                "playpos={}ms duration={}ms eos_pushed={} pipeline_eos={} ended={} \
                 clock={}ms running={running} stage_streaming={streaming} pending_video={pending} \
                 feed_allowed={} seek_pending={}",
                FEED_SHARED.playpos_ns.load(Relaxed) / 1_000_000,
                FEED_SHARED.duration_ns.load(Relaxed) / 1_000_000,
                self.eng().eos_pushed,
                FEED_SHARED.pipeline_eos.load(Relaxed),
                ended,
                clock_ns / 1_000_000,
                FEED_TX.feed_allowed(),
                seek_pending,
            )
        }
    }

    impl Drop for Rig {
        fn drop(&mut self) {
            ffi_host::reset_native_lifecycle_for_test();
            ffi_host::force_clocksink_for_test(false);
            FEED_SHARED.test_force_native_idle();
            crate::player::reset_session_for_test();
            FEED_TX.reset();
            crate::route::reset_player_control_for_test(crate::route::idle_session_for_test());
        }
    }

    #[test]
    fn a_film_whose_audio_outlasts_its_video_ends_on_the_host_sink() {
        let _serial = plx_base::testlock::serial();
        let mut rig = Rig::new();

        // The picture plays out to the last video frame with the demuxer still producing: the
        // sink has presented everything it was fed and has no end-of-stream to report.
        let at_the_tail = |_: &mut Rig| {
            let (position, running) = ffi_host::clock_state_for_test();
            running && position >= VIDEO_END_NS - 2 * V_STEP_NS
        };
        assert!(
            rig.run_until(at_the_tail),
            "PRECONDITION FAILED, not the defect: the clock never reached the last video frame \
             (the prime, the feed or the sink did not run): {}",
            rig.facts()
        );
        assert!(crate::player::seen_frame(&rig.ps) == Some(true), "PRECONDITION FAILED, not the defect: no picture was reported");
        for _ in 0..10 {
            rig.step();
        }
        assert!(
            !rig.eng().eos_pushed && !FEED_SHARED.pipeline_eos.load(Relaxed) && !crate::player::feed_ended(&rig.ps),
            "the sink reported an end of stream nobody pushed: {}",
            rig.facts()
        );

        // The file ends: the video lane drains and the engine pushes the EOS. The playhead is
        // stopped at the last picture, short of the duration by more than the tail, so the
        // arithmetic alone cannot end the film.
        rig.producer_done();
        assert!(
            rig.run_until(|r| r.eng().eos_pushed),
            "PRECONDITION FAILED, not the defect: the EOS was never pushed: {}",
            rig.facts()
        );
        let stalled = FEED_SHARED.playpos_ns.load(Relaxed);
        assert!(
            stalled < AUDIO_END_NS - EOS_TAIL_NS - S,
            "PRECONDITION FAILED, not the defect: the playhead ({stalled}ns) is within reach of the \
             duration, so the arithmetic would end this film"
        );
        assert!(
            rig.run_until(|r| crate::player::feed_ended(&r.ps)),
            "the film never ended: video ran to {}s, audio to {}s, and the pipeline's clock stopped \
             at the last picture: {}",
            VIDEO_END_NS / S,
            AUDIO_END_NS / S,
            rig.facts()
        );
        assert!(
            FEED_SHARED.pipeline_eos.load(Relaxed),
            "it ended on the pipeline's word, not on the arithmetic: {}",
            rig.facts()
        );
    }

    /// The event-log lines the era-callback watch wrote while `run` drove the rig.
    fn era_lines(run: impl FnOnce()) -> usize {
        plx_base::eventlog::with_private_log(|| {
            run();
            std::fs::read_to_string(plx_base::eventlog::events_log())
                .unwrap_or_default()
                .lines()
                .filter(|l| l.contains("era callbacks:"))
                .count()
        })
    }

    /// A played session that never receives the displayed-frame counter says so, once. The host
    /// sink sends position ticks and no counter, which is exactly the shape of a firmware whose
    /// numbering the app chose wrongly.
    #[test]
    fn a_played_session_without_the_displayed_counter_logs_one_line() {
        let _serial = plx_base::testlock::serial();
        let mut rig = Rig::new();
        let lines = era_lines(|| {
            assert!(
                rig.run_until(|_| FEED_SHARED.dg_pos_ticks.load(Relaxed) >= 80),
                "PRECONDITION FAILED, not the defect: the sink never took 80 position ticks: {}",
                rig.facts()
            );
        });
        assert_eq!(FEED_SHARED.dg_sink_displayed_cb.load(Relaxed), 0, "PRECONDITION FAILED, not the defect");
        assert_eq!(lines, 1, "a played session with no displayed counter logs exactly one line");
    }

    #[test]
    fn a_session_that_receives_the_displayed_counter_logs_nothing() {
        let _serial = plx_base::testlock::serial();
        let mut rig = Rig::new();
        let lines = era_lines(|| {
            for _ in 0..80 {
                rig.step();
                let epoch = rig.eng().native_epoch;
                let ty = plx_play_feed::sf_event::displayed_counter_type(plx_platform::tv::device::info().major);
                plx_play_feed::sf_event::sf_on_event(epoch, ty, 5, std::ptr::null());
            }
        });
        assert!(FEED_SHARED.dg_sink_displayed_cb.load(Relaxed) > 0, "PRECONDITION FAILED, not the defect: the counter was not counted");
        assert!(FEED_SHARED.dg_pos_ticks.load(Relaxed) >= 40, "PRECONDITION FAILED, not the defect: the session did not play");
        assert_eq!(lines, 0);
    }

    #[test]
    fn a_session_that_has_not_played_logs_nothing() {
        let _serial = plx_base::testlock::serial();
        let mut rig = Rig::new();
        let lines = era_lines(|| {
            for _ in 0..200 {
                let _frame = plx_base::task::FrameScope::enter();
                rig.now += 16;
                pump(&mut rig.ps, &mut rig.pa, rig.now);
            }
        });
        assert!(FEED_SHARED.dg_pos_ticks.load(Relaxed) < 25, "PRECONDITION FAILED, not the defect: the sink ticked on its own");
        assert_eq!(lines, 0);
    }
}
