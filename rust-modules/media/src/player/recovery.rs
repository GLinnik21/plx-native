//! The failed-open recovery ladder: what a play request does when the pipeline (or the source)
//! fails it BEFORE the first picture.
//!
//! `OpenRecovery` is a tested `State + RecoveryEvent -> Effects` machine in the shape of
//! [`super::lifecycle::ForegroundLifecycle`]: its state is private and its one writer is
//! [`step`](OpenRecovery::step). It reads no global and performs no IO. The facts a decision needs
//! (an HLS->Original trial holding a live encoder, a cold Auto Original, Forced Direct Play, no
//! decoded frame yet, the audio and the video the pipeline was fed, the natively fed audio a refusal names) arrive
//! as a value on the event ([`OpenFacts`]), and so do the verdicts PMS gave a landed flight
//! ([`Lanes`]); the work a decision asks for (the learner's write, a recovery flight, the failure
//! read-out) leaves as a [`RecoveryEffect`] which the pump (`super::pump`, the driver) executes
//! through the route's own dispatches, and every answer a flight gives returns as the next event in
//! the same drain.
//!
//! **The owner is the request.** [`PlaybackSession`](crate::route::PlaybackSession) holds one
//! `Option<OpenRecovery>`, created by `begin_attempt` with the capability value the app's play start
//! derived (`devcaps::start_caps`: the write-once boot snapshot less the install's stored refusals in
//! force), and replaced by the next request's: a refusal this request learned reaches the next one
//! only as a stored row, once the learner's write has landed. The machine also owns that value: the attempt's
//! capabilities are `start` narrowed by the facets the rungs took out of this request, at most one
//! audio and one video: the stream the facet rung converted, the refused video raster and native
//! audio a both-facets rung's attempt lacks (the HLS contingency or the manual encode), or the native audio a refusal named while a pending
//! rollback was chosen over the facet rung, so the restored route does not offer it again
//! ([`Caps::without`]), then the one lossy codec the app-decode rung put in force
//! ([`Caps::with_app_lossy_for`], applied last so a facet cannot take it out again);
//! [`attempt`](OpenRecovery::attempt) is the only reader the route plans from,
//! and `attempt` is derived from `start` on each rung's entry, never narrowed in place.
//!
//! **The ladder** is the order of [`decide`](OpenRecovery::decide): a pending HLS->Original
//! rollback first (its HLS route is the one a failed open owes the viewer); under Forced Direct
//! Play the app-decode rung (a track of at most two channels only) or the read-out (a forced play is never
//! converted, and the app never mixes a surround source down to stereo for it); else, for a refusal that names the
//! pipeline, the conversion of the stream the pipeline was fed when [`suspect`] names it (the facet
//! rung, once per request, [`RecoveryEffect::ConvertFacet`]: the audio when it is outside the codecs
//! every set decodes, else the source's video for a numbered refusal, which a cold Auto Original
//! leaves to its HLS rung); else the HLS contingency of a cold Auto Original (the both-facets rung,
//! [`RecoveryEffect::StartAutoHls`]), whose attempt lacks every facet the request took out and
//! every stream this refusal named, the refused video raster and the suspect audio; else, for a source of
//! at most two channels, the app decode ([`RecoveryEffect::DecodeInApp`]): a refused pipeline decode of a
//! lossy codec the start value's policy admits ([`Caps::app_may_decode_lossy`]) on a direct play with no
//! picture yet reloads the same play with the app decoding that audio to stereo PCM, no server asked and no
//! flight flown, once per request; else, for a manual pick (no HLS contingency retained), the manual
//! both-facets rung
//! ([`RecoveryEffect::ConvertBoth`]): one encode with no video copy, planned from `start` less the
//! audio suspect and the refused video raster, which a refusal of the pipeline earns while the source's
//! video is still fed and not yet encoded, or an audio suspect still narrows the attempt (a frame-rate
//! refusal of a 1080p source has nothing to lower and still gets the encode); else, for a source of MORE
//! than two channels (or of an unknown width), the app decode as the very last rung; else the failure
//! read-out. The owner's rule (2026-10-28) is that the app never mixes a surround source down to stereo by
//! default ([`plx_platform::devcaps::app_decode_mixes_down`] is its one predicate, shared with the route
//! planner): a stereo track's app decode comes before the encode because it keeps the direct play, asks the
//! server nothing and loses nothing; a surround track's comes after it, because the encode keeps the
//! channels; and a Forced play takes the app decode only for a stereo track, else the read-out, and is never
//! converted. The rungs which
//! start a flight (the facet rung and the both-facets rungs) are bounded by [`MAX_RECOVERY_FLIGHTS`] per
//! request. A timeout and a source failure name no stream, so a manual pick keeps main's read-out for
//! them, and an encode's own refusal reads out: it feeds an encoded video and no audio suspect. A Load
//! the app decoded for never proves a facet, nor does a flight after one (`unprovable`).
//!
//! **What the ladder teaches.** A refusal of natively fed audio (`num=200` or the stall) names the
//! codec, so the same step asks the driver to record it ([`RecoveryEffect::ConfirmRefusal`] with
//! [`Evidence::Named`]: the install's learner is IO) and then picks its rung; the write is
//! fire-and-forget, so no answer ever selects a rung (the facet rung is selected by [`suspect`] and
//! by the loop guard alone: a facet `start` already lacks narrows nothing and gets no flight, so a
//! stored refusal is never converted twice). A refusal that names nothing (`601`) teaches only
//! through a **proof**: the facet flight of an audio facet landed with PMS answering the converted
//! lane `transcode` and the other lane `copy`/`directplay` ([`proves`]), and the reload then showed
//! its first picture ([`State::Proving`], [`Evidence::Proof`]) of a request that had shown none and
//! with no second Load in between. A video facet flight, the both-facets rung, the rollback, a
//! timeout and a source failure never prove.
//!
//! **What the project hears.** A refused play that a rung then put a picture up for is one
//! [`RecoveryEffect::ReportRecovered`] per request ([`Recovered`]): the refusal the pipeline gave
//! and the stream it named, and the rung that played. The machine only says so; the driver hands it
//! to `player::report`, which sends nothing without the error-report consent.
//!
//! The state the machine does not own: the flight bookkeeping stays in `route::decision`
//! (`original_recovery_pending`, the recovery flight slot), and it reaches the machine as a fact
//! on the event, not as a field.

use std::sync::Arc;

use plx_machine::machine::Handled;
use plx_platform::devcaps::Caps;
use plx_platform::devcaps::Facet;

/// The most flights one play request starts through the facet rung and the both-facets rungs (and a
/// failed source's HLS contingency): the facet's conversion, then the HLS contingency of a cold Auto
/// Original or a manual pick's encode. The Original rollback is not counted.
pub const MAX_RECOVERY_FLIGHTS: u32 = 2;

/// How the open failed, as the pump's failure branches see it. The event payload; no field of the
/// session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoadRefusal {
    /// The pipeline's `type=18` error callback, with its number (`200` Audio Codec Not Supported,
    /// `601` Resource Allocation Error, ...). Also the synchronous `sf_load == 0` refusal, which
    /// publishes the same flag.
    Refused { num: i32 },
    /// The Load budget ran out with the pipeline silent.
    TimedOut,
    /// The surround native-audio watchdog fired: the pipeline took the Load and a few access units,
    /// then swallowed the feed with no picture ([`super::native_stall`]).
    Stalled,
    /// Not the pipeline: the demuxer or the transport died before a picture.
    SourceFailed,
}

impl LoadRefusal {
    /// Whether the refusal is the pipeline's, so the stream it was fed is a suspect. A timeout
    /// names no facet (the pipeline said nothing) and a source failure is not the pipeline's.
    pub fn names_pipeline(self) -> bool {
        matches!(self, Self::Refused { .. } | Self::Stalled)
    }

    /// Whether a rescue of this refusal is reported: the television's pipeline refused the Load
    /// (a number, or the surround stall) or answered nothing within the Load budget. A source
    /// failure is the demuxer's or the transport's, not the television's refusal.
    pub fn reports(self) -> bool {
        !matches!(self, Self::SourceFailed)
    }
}

impl std::fmt::Display for LoadRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused { num } => write!(f, "type=18 num={num}"),
            Self::TimedOut => f.write_str("Load timed out"),
            Self::Stalled => f.write_str("native audio stall"),
            Self::SourceFailed => f.write_str("source failed"),
        }
    }
}

/// The audio the pipeline was fed: the stream's declared codec (`route::stream_acodec`) and the
/// carried track's channel count (`route::stream_achannels`, 0 when unknown).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamAudio {
    /// The codec in any spelling [`plx_platform::devcaps::audio_codec_id`] knows (`dca` for DTS).
    pub codec: String,
    pub channels: u32,
    /// The app decoded this audio itself and fed the pipeline PCM (a lossy codec the attempt's value
    /// decodes in the app, `Caps::app_decodes_lossy_track`, `route::stream_pcm_is_lossy`): a refusal of
    /// the Load is then no evidence about the pipeline's own decoder for [`codec`](Self::codec), and
    /// the app-decode rung ([`RecoveryEffect::DecodeInApp`]) has nothing left to try.
    pub app_decoded_lossy: bool,
}

/// One lane of a `/decision` answer, as PMS's own `Stream.decision` says it (`copy`, `directplay`,
/// `transcode`); a body that is silent, or says anything else, is `Unstated`. The route builds it
/// (`route::plan::decision_lanes`); like [`StreamAudio`], it is machine vocabulary the route fills.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lane {
    DirectPlay,
    Copy,
    Transcode,
    Unstated,
}

impl Lane {
    /// The lane a wire `decision` string names.
    pub fn of(wire: &str) -> Lane {
        match wire {
            "directplay" => Lane::DirectPlay,
            "copy" => Lane::Copy,
            "transcode" => Lane::Transcode,
            _ => Lane::Unstated,
        }
    }
}

/// The verdicts of a landed flight's `/decision`, per lane.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lanes {
    pub video: Lane,
    pub audio: Lane,
}

/// Whether a landed flight that converted `facet` changed it ALONE, by PMS's own verdicts: the
/// facet's own lane is `transcode` AND the other lane is `copy`/`directplay`. Silence proves nothing.
fn proves(facet: &Facet, lanes: Option<&Lanes>) -> bool {
    let Some(l) = lanes else { return false };
    match facet {
        Facet::Audio { .. } => l.audio == Lane::Transcode && matches!(l.video, Lane::Copy | Lane::DirectPlay),
        // A video facet never proves (D6(c)): a rescue by encoding says the raster was the problem
        // only if the encode kept the frame rate, and nothing here can tell. It is the attempt's
        // alone and is never stored.
        Facet::Video { .. } => false,
    }
}

/// The video the pipeline was fed: the SOURCE's codec (`route::stream_vcodec`) and raster
/// (`route::stream_video`, `None` when the video was re-encoded or its raster is unknown).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamVideo {
    /// The codec in PMS's lower-case spelling (`h264`, `hevc`, `vp9`, `av1`).
    pub codec: String,
    pub width: u32,
    pub height: u32,
}

/// The audio a pipeline refusal puts under suspicion: outside the codecs every set decodes (`aac`,
/// `ac3`, `eac3`). `channels == 0` (unknown) names the codec at no width, which [`Caps::without`]
/// reads as "not at all", the conservative choice. Audio the app decoded itself
/// ([`StreamAudio::app_decoded_lossy`]) names nothing: the pipeline was fed PCM, and a facet of
/// that codec would narrow, and could store as a refusal, the set's NATIVE decoder.
fn suspect_audio(audio: Option<&StreamAudio>) -> Option<Facet> {
    let a = audio.filter(|a| !a.app_decoded_lossy)?;
    let id = plx_platform::devcaps::audio_codec_id(&a.codec);
    (!matches!(&*id, "aac" | "ac3" | "eac3")).then(|| Facet::Audio { codec: id.into_owned(), channels: a.channels })
}

/// The video a pipeline refusal puts under suspicion: the source's, at a raster the pipeline was fed
/// whole. An unknown raster (either side 0) names no facet, as the planner's direct-play gate passes
/// one it was never told.
fn suspect_video(video: Option<&StreamVideo>) -> Option<Facet> {
    let v = video.filter(|v| v.width > 0 && v.height > 0)?;
    Some(Facet::Video { codec: v.codec.to_ascii_lowercase(), width: v.width, height: v.height })
}

/// The stream class a pipeline refusal puts under suspicion, chosen by elimination: the audio when
/// it is outside the codecs every set decodes ([`suspect_audio`]; the video is then not
/// considered), else the `video` the pipeline was fed ([`suspect_video`]).
pub fn suspect(audio: Option<&StreamAudio>, video: Option<&StreamVideo>) -> Option<Facet> {
    suspect_audio(audio).or_else(|| suspect_video(video))
}

/// What the driver read, at the moment of a failure, that the ladder decides on.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OpenFacts {
    /// An HLS->Original trial holds a live encoder for exactly this outcome
    /// (`route::original_recovery_pending`).
    pub pending_original: bool,
    /// The playing item is a cold Auto Original (`route::auto_original_watch(..)` is some).
    pub auto_original: bool,
    /// No picture has decoded yet in this Engine (`frames == 0`).
    pub no_frames: bool,
    /// The natively fed audio this failure names: the pipeline refused it (`type=18 num=200`) or
    /// swallowed it (the stall). `None` when the failure does not name audio. It asks for the
    /// learner's write ([`RecoveryEffect::ConfirmRefusal`], [`Evidence::Named`]) and selects no rung.
    pub native_audio: Option<Facet>,
    /// Forced Direct Play (`route::forced_direct_play`): the viewer asked not to convert.
    pub forced: bool,
    /// The route is a direct play: the file's own bytes, with no server session between
    /// (`!route::is_transcoding`). Only such a route can be re-opened with the app decoding its audio
    /// ([`RecoveryEffect::DecodeInApp`]).
    pub direct: bool,
    /// What the pipeline was fed (`route::stream_audio`), for [`suspect`].
    pub audio: Option<StreamAudio>,
    /// The SOURCE's video, when the pipeline was fed it whole (`route::stream_video`: a direct play
    /// or a remux that copies it), for [`suspect`]. `None` for an encoded video.
    pub video: Option<StreamVideo>,
}

/// What moves the ladder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecoveryEvent {
    /// The open failed. Consumed in `Opening`, `Proving` and `Playing` (a failure after a picture
    /// runs the same ladder, whose `no_frames` fact then stops it at the failure read-out); a failure
    /// while a flight or the read-out is already outstanding is not.
    Refused { refusal: LoadRefusal, facts: OpenFacts },
    /// The recovery flight installed its replacement route; the driver reloads onto it. `lanes` are
    /// the verdicts of the `/decision` that built it (`None` for a landing which carries none: a
    /// rollback, the HLS contingency, a resume), the facet rung's proof ([`proves`]).
    FlightLanded { lanes: Option<Lanes> },
    /// The recovery flight could not start, or came back refused.
    FlightRefused,
    /// The first decoded picture of the open.
    FirstPicture,
    /// A Load started on this request's route (`engine::start_bufferfeed_for`, which every Engine
    /// start passes: a fresh play, a reload, the foreground restore's Load). A start the machine
    /// asked for has already moved it (`FlightLanded` -> `Opening`, or `Proving` for a clean facet
    /// landing), so this changes nothing in `Opening`, and the first one in `Proving` is the
    /// landing's own reload; a start it did not ask for (a park and restore, or any reload, that
    /// crossed an outstanding flight or the read-out) leaves `Flight` or `Failed` with a new open
    /// to rule on, and a second one in `Proving` drops the pending proof.
    LoadStarted,
}

/// What the driver executes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecoveryEffect {
    /// Record `facet` as refused on this firmware (the install's learner, which is IO and so the
    /// driver's): a refusal that named it ([`Evidence::Named`], applied at once) or a facet-clean
    /// rescue ([`Evidence::Proof`], counted). Fire-and-forget: nothing answers.
    ConfirmRefusal { facet: Facet, evidence: Evidence },
    /// Fly the HLS rollback of the failed Original trial (`dispatch_rollback_rebase`).
    Rollback,
    /// Fly today's route rebuilt from [`OpenRecovery::attempt`], the facet excluded: the server
    /// converts the stream the attempt no longer feeds (`dispatch_refusal_conversion`; a video facet
    /// is an encode with no video copy, an audio facet keeps the route as planned). `facet` is
    /// the stream class taken out of the attempt and `flight` the ordinal of this flight among the
    /// request's ([`MAX_RECOVERY_FLIGHTS`]), for the driver's log line.
    ConvertFacet { facet: Facet, flight: u32 },
    /// Fly today's route as an ENCODE with no video copy, planned from [`OpenRecovery::attempt`]: the
    /// manual both-facets rung (`dispatch_refusal_conversion` with `VideoLane::Both`; the audio stays
    /// copied or converted as the attempt plans it). The attempt lacks every stream this refusal named
    /// that narrows it (the suspect audio, and the refused video raster of a numbered refusal); with
    /// nothing to lower (a frame-rate refusal of a 1080p source) the encode alone is the rung.
    /// `flight` is the ordinal of this flight among the request's.
    ConvertBoth { flight: u32 },
    /// Fly the HLS contingency a cold Auto Original retained (`dispatch_unopened_auto_hls`).
    StartAutoHls,
    /// Reload the SAME direct play with the app decoding `audio` itself (stereo 48 kHz PCM) instead
    /// of the pipeline's decoder or the server: the app-decode rung (the very last one for a source of
    /// more than two channels, which the app would mix down; a stereo source's comes before the manual
    /// encode), no server is asked and no flight is flown. The attempt ([`OpenRecovery::attempt`]) has already put the codec in force. `refusal`
    /// is the failure that led here, for the driver's log line.
    DecodeInApp { audio: Facet, refusal: LoadRefusal },
    /// Forced Direct Play ends on the read-out after a pipeline refusal of an audio track outside the
    /// codecs every set decodes (the scheme of discussion #695: "not played, and the codec is named").
    /// LG's error 601 names no codec, so the driver names the track the Load was fed (`codec` is its
    /// raw spelling) as the request's play verdict, and `ShowError` follows.
    NameRefusedAudio { codec: String },
    /// Fail the Engine for good and show the failure read-out.
    ShowError { refusal: LoadRefusal },
    /// A refused play showed its first picture on a rung of the ladder: tell the project, once per
    /// request ([`Recovered`]). Not an answer to anything: the driver hands it to the report path
    /// (`player::report::refusal_recovered`), which itself sends nothing without the error-report
    /// consent.
    ReportRecovered(Recovered),
}

/// What a rescued refusal is reported as ([`RecoveryEffect::ReportRecovered`]): the request's first
/// pipeline refusal and the stream it put under suspicion, and the rung whose route put the
/// picture up. Machine vocabulary: the driver's report maps it onto the wire's closed classes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Recovered {
    /// The rung of the flight the picture came from (a later rung replaces an earlier one).
    pub rung: Rung,
    /// The first refusal of the request that came from the television's pipeline
    /// ([`LoadRefusal::reports`]); a later refusal on the way to the rescue does not replace it.
    pub refusal: LoadRefusal,
    /// The stream that refusal put under suspicion: the natively fed audio it named, else
    /// [`suspect`]'s answer (the video only for a numbered refusal). `None` when it named none.
    pub facet: Option<Facet>,
}

/// What a [`RecoveryEffect::ConfirmRefusal`] rests on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Evidence {
    /// The refusal names the codec (`num=200` or the stall; `OpenFacts::native_audio`).
    Named,
    /// A facet-clean rescue: the facet flight changed only that facet and its reload showed a picture.
    Proof,
}

/// Which rung a flight is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rung {
    /// The Original trial's HLS rollback.
    Rollback,
    /// The facet rung: the suspect stream converted.
    Facet,
    /// The HLS contingency of a cold Auto Original (the both-facets rung), planned from `start` less
    /// every facet the request took out (the facet rung's, when it flew first) and every stream this
    /// refusal named (the refused video raster, the suspect audio).
    Both,
    /// The manual both-facets rung: a manual pick's encode with no video copy, planned from `start`
    /// less the same facets. It plays as the report's own class (`encode`), not as `Both`'s HLS.
    Encode,
}

#[derive(Clone, Debug)]
enum State {
    /// The open is under way (the initial state, and the state a landed flight's reload re-enters,
    /// or `Proving` for a clean facet landing).
    Opening,
    /// The facet flight landed with a clean verdict ([`proves`]) and its reload is opening: `Opening`
    /// plus a pending proof of `facet`, credited at the first picture. `loaded` is whether the
    /// landing's own reload has started (`FlightLanded` moves the machine BEFORE the driver
    /// reloads, so the first `LoadStarted` here is that reload); a second Load before the picture is
    /// a route whose lanes were never read, and drops the proof.
    Proving { facet: Facet, loaded: bool },
    /// A recovery flight is outstanding; `cause` is the refusal that started it and `facts` are what
    /// the next rung decides on if the flight comes back refused.
    Flight { rung: Rung, cause: LoadRefusal, facts: OpenFacts },
    /// A picture decoded in this request. The ladder is not over: a reload starts a new Engine
    /// whose own open has no picture, and the facts (`no_frames`) say which Engine a refusal is of.
    Playing,
    /// The ladder is exhausted and the read-out was asked for.
    Failed,
}

/// The ladder for one play request. See the module doc.
#[derive(Clone, Debug)]
pub struct OpenRecovery {
    /// What the app's play start derived (`devcaps::start_caps`); never changed.
    start: Arc<Caps>,
    /// `start` narrowed by `excluded`: the value the route plans and profiles from. Assigned only
    /// on a rung's entry, always from `start` ([`attempt_for`]).
    attempt: Arc<Caps>,
    /// The facets the rungs took out, one per stream class (an audio and a video at most); carried
    /// into the both-facets rungs.
    excluded: Excluded,
    /// Flights started by the facet and both-facets rungs (and a failed source's HLS contingency).
    /// The rollback is not counted.
    flights: u32,
    /// Whether a picture has shown in this request (any handled `FirstPicture`). A request that
    /// pictured has played what it was fed, so a later rescue by conversion proves nothing about
    /// the converted facet: the proof is only for a request whose FIRST open the conversion rescued.
    pictured: bool,
    /// The codec the app-decode rung put in force ([`attempt_for`]); `Some` = the rung is spent, which
    /// is once per request.
    in_app: Option<String>,
    /// A refused attempt was an app decode: a later clean landing proves nothing about any facet.
    unprovable: bool,
    /// The request's first refusal that [`LoadRefusal::reports`], with the stream it named; kept for
    /// the request so the rescue's report names the refusal and not the failure of a rescue
    /// attempt.
    refused: Option<(LoadRefusal, Option<Facet>)>,
    /// The rung whose route the open now rests on: set when a flight starts, cleared when the
    /// ladder reads out or a Load the machine did not ask for abandons the flight. The first
    /// picture while this is set is a rescue.
    rescue: Option<Rung>,
    /// A rescue was reported for this request; at most one report per request.
    reported: bool,
    state: State,
}

/// The facets a request has taken out of its attempt: at most one per stream class, a later one
/// replacing the earlier of its class.
#[derive(Clone, Debug, Default)]
struct Excluded {
    audio: Option<Facet>,
    video: Option<Facet>,
}

impl Excluded {
    fn take(&mut self, facet: Facet) {
        match facet {
            Facet::Audio { .. } => self.audio = Some(facet),
            Facet::Video { .. } => self.video = Some(facet),
        }
    }

    fn iter(&self) -> impl Iterator<Item = &Facet> {
        self.audio.iter().chain(self.video.iter())
    }
}

/// `start` less `excluded`. Never takes a previous attempt as input, so the value is the same
/// whichever rungs ran.
fn attempt_for(start: &Caps, excluded: &Excluded, in_app: Option<&str>) -> Caps {
    let narrowed = excluded.iter().fold(start.clone(), |caps, facet| caps.without(facet));
    // After the facets, so the codec the app decodes is announced again whatever a facet took out.
    in_app.map_or(narrowed.clone(), |codec| narrowed.with_app_lossy_for(codec))
}

impl OpenRecovery {
    /// A fresh request on `start`.
    pub fn new(start: Arc<Caps>) -> Self {
        Self {
            attempt: start.clone(),
            start,
            excluded: Excluded::default(),
            flights: 0,
            pictured: false,
            in_app: None,
            unprovable: false,
            refused: None,
            rescue: None,
            reported: false,
            state: State::Opening,
        }
    }

    /// The capability value this request plans and profiles from: `start` less the facets the rungs
    /// took out. A cheap `Arc` clone, so a worker or a flight can own it.
    pub fn attempt(&self) -> Arc<Caps> {
        self.attempt.clone()
    }

    /// Re-derive the attempt from `start` and `excluded`; a rung's entry is the only caller.
    fn narrow(&mut self) {
        self.attempt = Arc::new(attempt_for(&self.start, &self.excluded, self.in_app.as_deref()));
    }

    /// The codec the app-decode rung would put in force for this failure, when the rung is available:
    /// a pipeline refusal of a direct play that has shown no picture, whose audio the pipeline itself
    /// decoded (not PCM the app fed), of a lossy codec the start value's policy admits, with the rung
    /// not yet spent in this request. The app decodes to stereo, so a source of more than two channels
    /// (or of an unknown width) is never decoded under Force Direct Play
    /// ([`Caps::app_may_mix_down`]: the owner's rule, the one the route planner's Forced passes use);
    /// Auto reaches this rung only as the last one ([`Self::decide`]).
    fn app_decode_for(&self, refusal: LoadRefusal, facts: &OpenFacts) -> Option<Facet> {
        let audio = facts.audio.as_ref().filter(|a| !a.app_decoded_lossy)?;
        let id = plx_platform::devcaps::audio_codec_id(&audio.codec);
        (self.in_app.is_none()
            && refusal.names_pipeline()
            && facts.no_frames
            && facts.direct
            && self.start.app_may_decode_lossy(&id)
            && self.start.app_may_mix_down(i64::from(audio.channels), facts.forced))
        .then(|| Facet::Audio { codec: id.into_owned(), channels: audio.channels })
    }

    /// Fire the app-decode rung when it is available, else the failure read-out. Forced's ladder and a
    /// refused both-facets flight end here.
    fn app_decode_or_error(&mut self, refusal: LoadRefusal, facts: &OpenFacts, fx: &mut Vec<RecoveryEffect>) {
        if self.try_app_decode(refusal, facts, fx) {
            return;
        }
        fx.push(RecoveryEffect::ShowError { refusal });
        self.state = State::Failed;
        self.rescue = None;
    }

    /// Fire the app-decode rung when it is available and say so. The one place the rung's entry is
    /// written: `in_app` set, the video facet cleared (the reload feeds the same source video, and a
    /// lowered raster ceiling would shrink the Load's video envelope for nothing), the attempt
    /// re-narrowed, no rescue to report (no flight flew), then the effect.
    fn try_app_decode(&mut self, refusal: LoadRefusal, facts: &OpenFacts, fx: &mut Vec<RecoveryEffect>) -> bool {
        let Some(audio) = self.app_decode_for(refusal, facts) else {
            return false;
        };
        if let Facet::Audio { codec, .. } = &audio {
            self.in_app = Some(codec.clone());
        }
        self.excluded.video = None;
        self.narrow();
        self.state = State::Opening;
        self.rescue = None;
        fx.push(RecoveryEffect::DecodeInApp { audio, refusal });
        true
    }

    /// The manual both-facets rung, when it is earned: one encode with no video copy, the audio the
    /// attempt lacks converted with it. It fires from the facts alone, at most once: every both flight
    /// leaves an encoded video (`facts.video == None`) and its audio suspects taken, and the video
    /// facet's identical encode set `excluded.video`. It still fires for a refusal with no raster to
    /// lower (a 1080p frame rate): the video is fed at a known raster and nothing was excluded. Not for
    /// a cold Auto Original (its HLS rung is the video re-encode) or an Engine that showed a picture.
    fn try_convert_both(&mut self, refusal: LoadRefusal, facts: &OpenFacts, fx: &mut Vec<RecoveryEffect>) -> bool {
        if facts.auto_original || !facts.no_frames || !refusal.names_pipeline() || self.flights >= MAX_RECOVERY_FLIGHTS {
            return false;
        }
        let audio = suspect_audio(facts.audio.as_ref()).filter(|f| self.attempt.without(f) != *self.attempt);
        let video = matches!(refusal, LoadRefusal::Refused { .. })
            .then(|| suspect_video(facts.video.as_ref()))
            .flatten()
            .filter(|f| self.attempt.without(f) != *self.attempt);
        let new_video = suspect_video(facts.video.as_ref()).is_some() && self.excluded.video.is_none();
        if !new_video && audio.is_none() {
            return false;
        }
        for facet in audio.into_iter().chain(video) {
            self.excluded.take(facet);
        }
        self.narrow();
        self.flights += 1;
        fx.push(RecoveryEffect::ConvertBoth { flight: self.flights });
        self.state = State::Flight { rung: Rung::Encode, cause: refusal, facts: facts.clone() };
        self.rescue = Some(Rung::Encode);
        true
    }

    /// The codec Force Direct Play's read-out may name for `refusal`, or `None` when the refusal is not
    /// attributable to the audio track (the read-out then stays the generic one). LG's `num=601`
    /// names no stream and is also a video/resource refusal, so the audio is named on positive
    /// evidence from this attempt's own value ([`OpenRecovery::attempt`]), never by elimination:
    /// * a pipeline refusal of an Engine that showed no picture, with audio [`suspect_audio`] puts under
    ///   suspicion (not AAC/AC-3/E-AC-3, not the app's own lossy decode);
    /// * not a lossless PCM codec ([`plx_platform::devcaps::is_pcm_codec`]): the app decodes it and the
    ///   TV was fed PCM (scheme row 6);
    /// * a codec the set is NOT expected to play from the file under Force
    ///   ([`Caps::native_audio_track`] false): Opus on a table that lists it and DTS on a set with a
    ///   DTS row play (rows 2 and 3), so a refusal there says nothing about them;
    /// * and, for a numbered refusal, a video that is not itself a suspect: the test the facet rungs
    ///   use (`Caps::without` of its [`suspect_video`] changes the attempt, i.e. the raster is above
    ///   [`plx_platform::devcaps::VIDEO_REFUSAL_FLOOR`], where the H.264 4K60 and 4K@30-limited
    ///   `num=601` refusals live, `engine::sink_envelope`). A `Stalled` Load is audio's alone.
    fn refusal_attributable_to_audio(&self, refusal: LoadRefusal, facts: &OpenFacts) -> Option<String> {
        if !refusal.names_pipeline() || !facts.no_frames {
            return None;
        }
        suspect_audio(facts.audio.as_ref())?;
        let a = facts.audio.as_ref()?;
        let set_plays_it = self.attempt.native_audio_track(&a.codec, i64::from(a.channels), 0, true);
        let video_may_be_it = matches!(refusal, LoadRefusal::Refused { .. })
            && suspect_video(facts.video.as_ref()).is_some_and(|f| self.attempt.without(&f) != *self.attempt);
        (!plx_platform::devcaps::is_pcm_codec(&a.codec) && !set_plays_it && !video_may_be_it).then(|| a.codec.clone())
    }

    /// The rung `facts` select. The order is the proof that no rung of main is reordered: pending
    /// rollback, Forced's app decode (a track of at most two channels) or read-out, the facet rung, a cold
    /// Auto Original's HLS rung, then for a track of at most two channels the app decode and a manual
    /// pick's encode, or for a surround track the encode and then the app decode (the owner's rule: the
    /// app mixes a surround source down only as the last rung), the read-out. The facet
    /// rung is for an Engine that has shown no picture (`facts.no_frames`: the picture is per
    /// Engine, so a reload after the request's first picture owes its own open the same rung).
    fn decide(&mut self, refusal: LoadRefusal, facts: &OpenFacts, fx: &mut Vec<RecoveryEffect>) {
        let flight = |rung| State::Flight { rung, cause: refusal, facts: facts.clone() };
        self.note_refusal(refusal, facts);
        if facts.pending_original {
            // A refusal of native audio (the learner's write was queued earlier in this step)
            // narrows the rollback's profile too: the restored HLS route must not offer it natively
            // again.
            if let Some(facet) = facts.native_audio.as_ref().filter(|f| self.start.without(f) != *self.start) {
                self.excluded.audio = Some(facet.clone());
                self.narrow();
            }
            fx.push(RecoveryEffect::Rollback);
            self.state = flight(Rung::Rollback);
            self.rescue = Some(Rung::Rollback);
            return;
        }
        if facts.forced {
            // A forced play is never converted, but the app can decode a stereo track the set refused (a
            // surround track is not mixed down for it: `app_decode_for` declines, and the read-out follows).
            if self.try_app_decode(refusal, facts, fx) {
                return;
            }
            if let Some(codec) = self.refusal_attributable_to_audio(refusal, facts) {
                fx.push(RecoveryEffect::NameRefusedAudio { codec });
            }
            fx.push(RecoveryEffect::ShowError { refusal });
            self.state = State::Failed;
            self.rescue = None;
            return;
        }
        if refusal.names_pipeline() && facts.no_frames && self.flights == 0 {
            // The video facet rung runs only where no HLS contingency is retained: a cold Auto
            // Original's HLS rung is already the video re-encode (ABR-managed), and only a refusal
            // that gave a number can name a decode (`Stalled` is native audio's).
            let video = facts.video.as_ref().filter(|_| !facts.auto_original && matches!(refusal, LoadRefusal::Refused { .. }));
            if let Some(facet) = suspect(facts.audio.as_ref(), video).filter(|f| self.start.without(f) != *self.start) {
                // A flight after an app decode never proves a facet (`unprovable`).
                self.unprovable |= facts.audio.as_ref().is_some_and(|a| a.app_decoded_lossy);
                self.excluded.take(facet.clone());
                self.narrow();
                self.flights += 1;
                fx.push(RecoveryEffect::ConvertFacet { facet, flight: self.flights });
                self.state = flight(Rung::Facet);
                self.rescue = Some(Rung::Facet);
                return;
            }
        }
        if facts.auto_original && facts.no_frames && self.flights < MAX_RECOVERY_FLIGHTS {
            // The HLS attempt lacks every stream the refusal named that narrows `start`: the refused
            // video raster (a numbered refusal) and native audio (any pipeline refusal). A timeout or
            // a source failure names none, so main's attempt flies.
            let audio = refusal.names_pipeline().then(|| suspect_audio(facts.audio.as_ref())).flatten();
            let video = matches!(refusal, LoadRefusal::Refused { .. }).then(|| suspect_video(facts.video.as_ref())).flatten();
            for facet in audio.into_iter().chain(video).filter(|f| self.start.without(f) != *self.start) {
                self.excluded.take(facet);
            }
            self.narrow();
            self.flights += 1;
            fx.push(RecoveryEffect::StartAutoHls);
            self.state = flight(Rung::Both);
            self.rescue = Some(Rung::Both);
            return;
        }
        // The app decodes to stereo. For a source of at most two channels that loses nothing and keeps
        // the direct play and asks the server nothing, so it comes before the manual encode (a refused
        // conversion of the audio is decoded in the app, not converted twice). A surround source would
        // lose its channels, so there (owner decision 2026-10-28) the encode, which keeps them, comes
        // first and the app decode is the very last rung.
        let mixes_down = facts.audio.as_ref().is_some_and(|a| plx_platform::devcaps::app_decode_mixes_down(i64::from(a.channels)));
        if !mixes_down && self.try_app_decode(refusal, facts, fx) {
            return;
        }
        if self.try_convert_both(refusal, facts, fx) {
            return;
        }
        if mixes_down && self.try_app_decode(refusal, facts, fx) {
            return;
        }
        fx.push(RecoveryEffect::ShowError { refusal });
        self.state = State::Failed;
        self.rescue = None;
    }

    /// Keep the request's first refusal the report can name ([`LoadRefusal::reports`]) and the
    /// stream it put under suspicion: for a refusal that names the pipeline, the natively fed audio
    /// it named, else [`suspect`] (the video only for a numbered refusal, which is what can name a
    /// decode); none for a Load timeout. Later refusals, which are the failures of rescue
    /// attempts, never replace it.
    fn note_refusal(&mut self, refusal: LoadRefusal, facts: &OpenFacts) {
        if self.refused.is_some() || !refusal.reports() {
            return;
        }
        // Only a refusal that names the pipeline puts a stream under suspicion: a Load timeout says
        // nothing about the audio fed, and the ladder itself ignores it for one.
        let facet = refusal.names_pipeline().then(|| {
            let video = facts.video.as_ref().filter(|_| matches!(refusal, LoadRefusal::Refused { .. }));
            facts.native_audio.clone().or_else(|| suspect(facts.audio.as_ref(), video))
        });
        self.refused = Some((refusal, facet.flatten()));
    }

    /// The first picture of an open that rests on a rung's route: a rescued refusal, reported once
    /// per request.
    fn report_rescue(&mut self, fx: &mut Vec<RecoveryEffect>) {
        let rung = self.rescue.take();
        if self.reported {
            return;
        }
        if let (Some(rung), Some((refusal, facet))) = (rung, &self.refused) {
            self.reported = true;
            fx.push(RecoveryEffect::ReportRecovered(Recovered { rung, refusal: *refusal, facet: facet.clone() }));
        }
    }

    /// The one writer. The table (a pair not listed answers [`Handled::No`] and changes nothing):
    ///
    /// | state | event | effects, next state |
    /// |---|---|---|
    /// | `Opening`, `Proving`, `Playing` | `Refused`, facts name native audio `f` | `ConfirmRefusal{f, Named}`, then the ladder's effect; the ladder's state |
    /// | `Opening`, `Proving`, `Playing` | `Refused`, otherwise | the ladder below (`Playing` and `Opening` rule alike: the facts say which Engine failed; a pending proof is dropped) |
    /// | `Flight{Facet}` | `FlightLanded{lanes}`, an excluded facet [`proves`] (an audio facet only), no picture yet in the request | none; `Proving{that facet}` |
    /// | `Flight{Facet}` | `FlightLanded{..}`, otherwise | none; `Opening` (the reload re-enters the open) |
    /// | `Flight{Rollback, Both, Encode}` | `FlightLanded{..}` | none; `Opening` (lanes ignored: only the facet rung proves) |
    /// | `Flight{Facet}` | `FlightRefused` | the ladder below (one flight spent, so the facet rung is closed): `StartAutoHls` for a cold Auto Original, else for a track of at most two channels `DecodeInApp` where the rung is open, else `ConvertBoth` when the manual rung is earned (an audio facet's flight, the video still fed), else (a surround track) `DecodeInApp` where the rung is open, else `ShowError` |
    /// | `Flight{Both, Encode}` | `FlightRefused` | `DecodeInApp` where the rung is open, else `ShowError`; `Failed` |
    /// | `Flight{Rollback}` | `FlightRefused` | `ShowError`; `Failed` (a refused rollback stays terminal) |
    /// | `Opening` | `FirstPicture` | `ReportRecovered` when a rung's route is what opened (below), else none; `Playing` |
    /// | `Proving{f}` | `FirstPicture` | `ConfirmRefusal{f, Proof}`, then `ReportRecovered` (below); `Playing` |
    /// | `Proving`, first | `LoadStarted` | none; `Proving` (the landing's own reload) |
    /// | `Proving`, after its reload | `LoadStarted` | none; `Opening` (a foreign Load: the proof is dropped) |
    /// | `Flight`, `Failed` | `LoadStarted` | none; `Opening`, `flights` and the excluded facets kept, the rung forgotten |
    ///
    /// The ladder: `pending_original` -> `Rollback`; else `forced` -> `DecodeInApp` (at most two channels) or `ShowError`; else a pipeline
    /// refusal, of an Engine with no picture (`no_frames`), of a [`suspect`] stream which narrows the
    /// attempt (the audio outside `aac`/`ac3`/`eac3`; else, for a numbered refusal of a play that is
    /// not a cold Auto Original, the source's video), with no flight spent -> `ConvertFacet`; else
    /// `auto_original && no_frames` with a flight left -> `StartAutoHls`, its attempt lacking the
    /// audio and the video raster the refusal named (each: `Flight`); else, for a track of at most two
    /// channels, `DecodeInApp` (a pipeline
    /// refusal of a direct play with no picture, whose audio the pipeline itself decoded, of a lossy
    /// codec the start value admits, once per request: `Opening`, the attempt decoding that codec in
    /// the app and the refused video raster given back); else, not a cold Auto Original,
    /// a pipeline refusal of an Engine with no picture and a flight left, when the source's video
    /// is fed and not yet excluded or an audio suspect still narrows the attempt -> `ConvertBoth`
    /// (the audio suspect and, for a numbered refusal, the video raster taken out of the attempt:
    /// `Flight{Encode}`); else, for a track of MORE than two channels (or of unknown width), `DecodeInApp`
    /// as the last rung; else `ShowError`; `Failed`. The encode fed an encoded video and no audio
    /// suspect, so its own refusal reads out, or, where the rung is open, takes the app decode
    /// (`Flight{Encode}` + `FlightRefused`, above).
    ///
    /// **The report** ([`Recovered`]): the machine remembers the request's FIRST refusal that
    /// [`LoadRefusal::reports`] with the stream it named, and the rung of the flight the open now
    /// rests on (the latest rung, set when a flight starts and cleared by the read-out, by the app
    /// decode (it flies no flight and has no rung to report) and by a
    /// Load the machine did not ask for that abandons a flight which never landed). The first picture of such an open pushes one
    /// `ReportRecovered`, once per request. A play with no refusal, a source failure, a ladder
    /// that read out, an app decode and a flight Home abandoned report nothing.
    ///
    /// `Refused` is not handled in `Flight` or `Failed`: a flight is outstanding (the pump runs no
    /// failure arm while one is), or the read-out is already up (the pump meets the same Engine's
    /// failed flags every frame until the Engine is released, and re-asserts the read-out itself).
    /// `LoadStarted` is not handled in `Opening` or `Playing`: the landing's own reload fires it after
    /// `FlightLanded` has moved the machine, which is `Opening` there, and it is the first one in
    /// `Proving` (kept, with the pending proof). The driver treats an unhandled `Refused` as the
    /// Engine failing for good, never as silence.
    pub fn step(&mut self, ev: &RecoveryEvent, fx: &mut Vec<RecoveryEffect>) -> Handled {
        match ev {
            RecoveryEvent::Refused { refusal, facts } => {
                if !matches!(self.state, State::Opening | State::Proving { .. } | State::Playing) {
                    return Handled::No;
                }
                if let Some(facet) = &facts.native_audio {
                    fx.push(RecoveryEffect::ConfirmRefusal { facet: facet.clone(), evidence: Evidence::Named });
                }
                self.decide(*refusal, facts, fx);
                Handled::Yes
            }
            RecoveryEvent::FlightLanded { lanes } => {
                let State::Flight { rung, .. } = &self.state else {
                    return Handled::No;
                };
                let proven = (*rung == Rung::Facet && !self.pictured && !self.unprovable)
                    .then(|| self.excluded.iter().find(|f| proves(f, lanes.as_ref())))
                    .flatten();
                self.state = match proven {
                    Some(facet) => State::Proving { facet: facet.clone(), loaded: false },
                    None => State::Opening,
                };
                Handled::Yes
            }
            RecoveryEvent::FlightRefused => {
                let State::Flight { rung, cause, facts } = &self.state else {
                    return Handled::No;
                };
                let (rung, cause, facts) = (*rung, *cause, facts.clone());
                match rung {
                    Rung::Facet => self.decide(cause, &facts, fx),
                    // The HLS contingency or the manual encode came back refused: the app decode is what is
                    // left (for a surround source, where it was held back for last, the very last rung;
                    // an encode is not a direct play, so its own refusal otherwise reads out).
                    Rung::Both | Rung::Encode => self.app_decode_or_error(cause, &facts, fx),
                    // A refused rollback stays terminal.
                    Rung::Rollback => {
                        fx.push(RecoveryEffect::ShowError { refusal: cause });
                        self.state = State::Failed;
                        self.rescue = None;
                    }
                }
                Handled::Yes
            }
            RecoveryEvent::FirstPicture => match &self.state {
                State::Opening => {
                    self.pictured = true;
                    self.state = State::Playing;
                    self.report_rescue(fx);
                    Handled::Yes
                }
                State::Proving { facet, .. } => {
                    fx.push(RecoveryEffect::ConfirmRefusal { facet: facet.clone(), evidence: Evidence::Proof });
                    self.pictured = true;
                    self.state = State::Playing;
                    self.report_rescue(fx);
                    Handled::Yes
                }
                _ => Handled::No,
            },
            RecoveryEvent::LoadStarted => match &mut self.state {
                // The landing's own reload: `FlightLanded` moved the machine before the driver
                // reloaded, so the first Load here is the one the landing asked for.
                State::Proving { loaded: loaded @ false, .. } => {
                    *loaded = true;
                    Handled::Yes
                }
                // A second Load before the picture (a track switch, a restore after Home, a seek
                // reload) is a route whose lanes were never read: the proof is dropped, and the new
                // open is ruled on by the same ladder as below.
                State::Proving { .. } | State::Flight { .. } | State::Failed => {
                    // The flight (if any) is gone with the Engine that waited on it, and the
                    // read-out belongs to the Engine that failed: the new Load is a new open, ruled
                    // on by the same ladder with what this request has already spent (`flights`,
                    // `excluded`). A flight that never landed or a read-out leaves the new open on
                    // no rung's route, so a picture from it is no rescue; a `Proving` state's route
                    // HAS landed (a track pick or a restore reloads that same installed route), so
                    // it keeps its rung, and only its proof is dropped.
                    if !matches!(self.state, State::Proving { .. }) {
                        self.rescue = None;
                    }
                    self.state = State::Opening;
                    Handled::Yes
                }
                State::Opening | State::Playing => Handled::No,
            },
        }
    }

    /// Read-only view of the machine for tests; every write goes through [`step`](Self::step).
    #[cfg(any(test, feature = "test-support"))]
    pub fn phase(&self) -> Phase {
        match self.state {
            State::Opening => Phase::Opening,
            State::Proving { .. } => Phase::Proving,
            State::Flight { .. } => Phase::Flight,
            State::Playing => Phase::Playing,
            State::Failed => Phase::Failed,
        }
    }
}

/// [`OpenRecovery`]'s state, without its payloads, for tests.
#[cfg(any(test, feature = "test-support"))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Opening,
    Proving,
    Flight,
    Playing,
    Failed,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dts6() -> Facet {
        Facet::Audio { codec: "dts".into(), channels: 6 }
    }

    fn opus6() -> Facet {
        Facet::Audio { codec: "opus".into(), channels: 6 }
    }

    fn audio(codec: &str, channels: u32) -> Option<StreamAudio> {
        Some(StreamAudio { codec: codec.into(), channels, app_decoded_lossy: false })
    }

    /// The same track when the app decoded it (the attempt decodes it in the app): the pipeline was fed PCM.
    fn app_decoded(codec: &str, channels: u32) -> Option<StreamAudio> {
        Some(StreamAudio { codec: codec.into(), channels, app_decoded_lossy: true })
    }

    /// A set whose table lists DTS at six channels (the shape of `plan_track_selection_tests.rs`'
    /// `dts_row_caps`): taking `dts6()` out of it is a real narrowing.
    fn dts_start() -> Arc<Caps> {
        Arc::new(Caps {
            audio: "aac,ac3,eac3,dts".into(),
            audio_channels: [("dts".into(), 6)].into(),
            ..Caps::assumed()
        })
    }

    /// A set whose pipeline announces Opus at any channel count.
    fn opus_start() -> Arc<Caps> {
        Arc::new(Caps { audio: "aac,ac3,eac3,opus".into(), ..Caps::assumed() })
    }

    fn machine() -> OpenRecovery {
        OpenRecovery::new(dts_start())
    }

    fn facts(pending_original: bool, auto_original: bool, no_frames: bool) -> OpenFacts {
        OpenFacts { pending_original, auto_original, no_frames, ..OpenFacts::default() }
    }

    /// `f` with the DTS 5.1 track the pipeline was fed.
    fn on_dts(f: OpenFacts) -> OpenFacts {
        on_dts_ch(f, 6)
    }

    /// `f` with a DTS track of `channels` channels the pipeline was fed: the width decides whether the app's
    /// stereo decode is a downmix (`app_decode_mixes_down`).
    fn on_dts_ch(f: OpenFacts, channels: u32) -> OpenFacts {
        OpenFacts { audio: audio("dca", channels), ..f }
    }

    /// `f` with the Opus 5.1 track the pipeline was fed natively, which the refusal names.
    fn names_opus(f: OpenFacts) -> OpenFacts {
        OpenFacts { native_audio: Some(opus6()), audio: audio("opus", 6), ..f }
    }

    fn run(m: &mut OpenRecovery, ev: RecoveryEvent) -> (Handled, Vec<RecoveryEffect>) {
        let mut fx = Vec::new();
        let handled = m.step(&ev, &mut fx);
        (handled, fx)
    }

    /// `run`, less the rescue report: the tests of the PROOF read the learner's effects only (the
    /// report has its own tests below).
    fn run_proof(m: &mut OpenRecovery, ev: RecoveryEvent) -> (Handled, Vec<RecoveryEffect>) {
        let (handled, mut fx) = run(m, ev);
        fx.retain(|e| !matches!(e, RecoveryEffect::ReportRecovered(_)));
        (handled, fx)
    }

    const SHUT: LoadRefusal = LoadRefusal::Refused { num: 601 };
    const NATIVE: LoadRefusal = LoadRefusal::Refused { num: 200 };
    const ERROR: [RecoveryEffect; 1] = [RecoveryEffect::ShowError { refusal: SHUT }];

    /// Scheme #695 row 10 / "Force: anything else is not played": the read-out names the refused codec.
    fn forced_dts_error() -> Vec<RecoveryEffect> {
        vec![RecoveryEffect::NameRefusedAudio { codec: "dca".into() }, RecoveryEffect::ShowError { refusal: SHUT }]
    }

    fn refused(refusal: LoadRefusal, facts: OpenFacts) -> RecoveryEvent {
        RecoveryEvent::Refused { refusal, facts }
    }

    /// A landing which carries no verdicts (a rollback's, the HLS contingency's).
    fn landed() -> RecoveryEvent {
        RecoveryEvent::FlightLanded { lanes: None }
    }

    /// PMS answered the converted audio lane `transcode` and left the video lane alone.
    const CLEAN: Lanes = Lanes { video: Lane::Copy, audio: Lane::Transcode };

    fn landed_with(lanes: Lanes) -> RecoveryEvent {
        RecoveryEvent::FlightLanded { lanes: Some(lanes) }
    }

    fn proof(facet: Facet) -> RecoveryEffect {
        RecoveryEffect::ConfirmRefusal { facet, evidence: Evidence::Proof }
    }

    fn named(facet: Facet) -> RecoveryEffect {
        RecoveryEffect::ConfirmRefusal { facet, evidence: Evidence::Named }
    }

    /// The ladder by facts, through the machine: the rung a `601` lands on, with (`suspect`) or
    /// without a DTS 5.1 track fed.
    fn rung(pending: bool, suspect: bool, auto: bool, no_frames: bool) -> Vec<RecoveryEffect> {
        let f = facts(pending, auto, no_frames);
        run(&mut machine(), refused(SHUT, if suspect { on_dts(f) } else { f })).1
    }

    fn h264_4k() -> Option<StreamVideo> {
        Some(StreamVideo { codec: "h264".into(), width: 3840, height: 2160 })
    }

    fn h264_4k_facet() -> Facet {
        Facet::Video { codec: "h264".into(), width: 3840, height: 2160 }
    }

    /// The facts of a play whose pipeline was fed a 4K H.264 behind `fed` audio.
    fn on_4k(fed: &str, auto: bool) -> OpenFacts {
        OpenFacts { auto_original: auto, no_frames: true, audio: audio(fed, 6), video: h264_4k(), ..OpenFacts::default() }
    }

    fn manual_machine() -> OpenRecovery {
        OpenRecovery::new(Arc::new(Caps::assumed()))
    }

    /// [`dts_start`] on a table that was read: the lossy policy admits the measured rows (DTS).
    fn measured_start() -> Arc<Caps> {
        Arc::new(Caps { app_lossy: plx_platform::devcaps::AppLossy::Measured, ..(*dts_start()).clone() })
    }

    /// A machine on [`measured_start`].
    fn measured_machine() -> OpenRecovery {
        OpenRecovery::new(measured_start())
    }

    /// `f` for a direct play: the file's own bytes with no server session.
    fn direct(f: OpenFacts) -> OpenFacts {
        OpenFacts { direct: true, ..f }
    }

    /// The effect the rung asks for on a refused DTS 5.1 direct play.
    fn decode_dts(refusal: LoadRefusal) -> RecoveryEffect {
        decode_dts_ch(refusal, 6)
    }

    /// The effect the rung asks for on a refused DTS direct play of `channels` channels.
    fn decode_dts_ch(refusal: LoadRefusal, channels: u32) -> RecoveryEffect {
        RecoveryEffect::DecodeInApp { audio: Facet::Audio { codec: "dts".into(), channels }, refusal }
    }

    fn h264_1080p() -> Option<StreamVideo> {
        Some(StreamVideo { codec: "h264".into(), width: 1920, height: 1080 })
    }

    /// The facts of a manual play whose pipeline was fed `video` behind `fed` audio.
    fn manual_on(fed: &str, video: Option<StreamVideo>) -> OpenFacts {
        OpenFacts { no_frames: true, audio: audio(fed, 6), video, ..OpenFacts::default() }
    }

    /// The video is the suspect only when the audio is not: elimination, not the refusal's number.
    #[test]
    fn suspect_names_video_only_when_the_audio_is_not_the_suspect() {
        assert_eq!(suspect(audio("aac", 2).as_ref(), h264_4k().as_ref()), Some(h264_4k_facet()));
        assert_eq!(suspect(audio("dca", 6).as_ref(), h264_4k().as_ref()), Some(dts6()), "the audio first");
        let unknown = Some(StreamVideo { codec: "h264".into(), width: 0, height: 0 });
        assert_eq!(suspect(audio("ac3", 6).as_ref(), unknown.as_ref()), None, "an unknown raster names nothing");
        assert_eq!(suspect(audio("aac", 2).as_ref(), None), None);
        let upper = Some(StreamVideo { codec: "HEVC".into(), width: 3840, height: 2160 });
        assert_eq!(suspect(None, upper.as_ref()), Some(Facet::Video { codec: "hevc".into(), width: 3840, height: 2160 }));
    }

    /// A 4K H.264 the pipeline refuses (any number) in a manual pick is converted once: the video
    /// is encoded under the 1080p floor and the rung asks the dispatch for the encode lane.
    #[test]
    fn a_4k_h264_refused_in_a_manual_pick_converts_the_video_once() {
        let mut m = manual_machine();
        let (handled, fx) = run(&mut m, refused(LoadRefusal::Refused { num: 100 }, on_4k("ac3", false)));
        assert_eq!(handled, Handled::Yes);
        assert_eq!(fx, [RecoveryEffect::ConvertFacet { facet: h264_4k_facet(), flight: 1 }]);
        assert_eq!(m.phase(), Phase::Flight);
        assert_eq!(m.attempt().raster_bound("h264"), (1920, 1088));
        assert_eq!(m.attempt().raster_bound("hevc"), (3840, 2176), "another codec keeps its bound");
    }

    /// The audio rung comes first when the audio is the suspect. The remux that converted it still
    /// copies the 4K video, so its refusal takes the both-facets rung, which lowers the video as well.
    #[test]
    fn a_video_refusal_whose_audio_is_the_suspect_takes_the_audio_rung_first() {
        let mut m = machine();
        let cause = LoadRefusal::Refused { num: 100 };
        let (_, fx) = run(&mut m, refused(cause, on_4k("dca", false)));
        assert_eq!(fx, [RecoveryEffect::ConvertFacet { facet: dts6(), flight: 1 }]);
        assert_eq!(m.attempt().raster_bound("h264"), (3840, 2176), "the video is untouched");
        run(&mut m, landed());
        let (_, fx) = run(&mut m, refused(cause, on_4k("ac3", false)));
        assert_eq!(fx, [RecoveryEffect::ConvertBoth { flight: 2 }]);
        assert_eq!(m.phase(), Phase::Flight);
        assert_eq!(*m.attempt(), dts_start().without(&dts6()).without(&h264_4k_facet()));
    }

    /// **The manual both-facets rung.** A refusal with nothing to lower (a 1080p source: the frame-rate
    /// case) still gets one encode of the video, the attempt unchanged; the encode's own refusal reads
    /// out, because the route it flew feeds an encoded video (`video: None`) and no audio suspect.
    #[test]
    fn the_manual_both_facets_rung_converts_both_then_reads_out() {
        let start = Arc::new(Caps::assumed());
        let mut m = OpenRecovery::new(start.clone());
        let cause = LoadRefusal::Refused { num: 601 };
        let (handled, fx) = run(&mut m, refused(cause, manual_on("ac3", h264_1080p())));
        assert_eq!((handled, fx), (Handled::Yes, vec![RecoveryEffect::ConvertBoth { flight: 1 }]));
        assert_eq!(m.phase(), Phase::Flight);
        assert_eq!(*m.attempt(), *start, "nothing to lower");
        run(&mut m, landed());
        assert_eq!(m.phase(), Phase::Opening);
        let (_, fx) = run(&mut m, refused(cause, manual_on("ac3", None)));
        assert_eq!((fx, m.phase()), (vec![RecoveryEffect::ShowError { refusal: cause }], Phase::Failed));
    }

    /// A stall names native audio alone, so it never lowers the video, but it is a pipeline refusal
    /// like any other and the encode is still worth one flight.
    #[test]
    fn a_stall_takes_the_both_facets_rung_without_lowering_the_video() {
        let start = Arc::new(Caps::assumed());
        let mut m = OpenRecovery::new(start.clone());
        let (_, fx) = run(&mut m, refused(LoadRefusal::Stalled, manual_on("ac3", h264_4k())));
        assert_eq!(fx, [RecoveryEffect::ConvertBoth { flight: 1 }]);
        assert_eq!(*m.attempt(), *start, "a stall names no video raster");
    }

    /// A refused audio flight with the source video still fed takes the both rung: the audio the
    /// attempt lacks AND the video are converted together.
    #[test]
    fn a_refused_audio_flight_dispatch_takes_the_both_facets_rung() {
        let mut m = machine();
        run(&mut m, refused(SHUT, OpenFacts { video: h264_4k(), ..on_dts(facts(false, false, true)) }));
        let (_, fx) = run(&mut m, RecoveryEvent::FlightRefused);
        assert_eq!(fx, [RecoveryEffect::ConvertBoth { flight: 2 }]);
        assert_eq!(*m.attempt(), dts_start().without(&dts6()).without(&h264_4k_facet()));
    }

    /// The video facet flight and the both rung share the cap: video facet, both rung, then the read-out.
    #[test]
    fn the_both_rung_after_the_video_facet_needs_an_audio_suspect_then_the_cap_holds() {
        let mut m = manual_machine();
        let cause = LoadRefusal::Refused { num: 100 };
        run(&mut m, refused(cause, on_4k("ac3", false)));
        run(&mut m, landed());
        // The encode feeds an encoded video: no video fact, no audio suspect: the read-out.
        let (_, fx) = run(&mut m.clone(), refused(cause, manual_on("ac3", None)));
        assert_eq!(fx, [RecoveryEffect::ShowError { refusal: cause }]);
        // An audio suspect in the encode's feed narrows the attempt: the both rung (flight 2) ...
        let start = Arc::new(Caps { audio: "aac,ac3,eac3,opus".into(), ..Caps::assumed() });
        let mut m = OpenRecovery::new(start);
        run(&mut m, refused(cause, on_4k("ac3", false)));
        run(&mut m, landed());
        let (_, fx) = run(&mut m, refused(cause, manual_on("opus", None)));
        assert_eq!(fx, [RecoveryEffect::ConvertBoth { flight: 2 }]);
        run(&mut m, landed());
        // ... and a third refusal finds the cap spent.
        let (_, fx) = run(&mut m, refused(cause, manual_on("opus", h264_4k())));
        assert_eq!(fx, [RecoveryEffect::ShowError { refusal: cause }]);
    }

    /// The picture is per ENGINE (`no_frames`), not per request: a request that pictured and then
    /// reloads (a resume after Home, a track pick) owes the new Engine's open the same rung, so its
    /// pre-picture refusal earns the encode, and a refusal of an Engine that HAS a picture does not.
    /// A resume refusal is the likelier transient one, but nothing in the design exempts it: a known
    /// gap, pinned here as the behaviour.
    #[test]
    fn the_both_rung_follows_the_engines_picture_not_the_requests() {
        let cause = LoadRefusal::Refused { num: 601 };
        let mut m = manual_machine();
        run(&mut m, RecoveryEvent::FirstPicture);
        let (_, fx) = run(&mut m, refused(cause, manual_on("ac3", h264_1080p())));
        assert_eq!(fx, [RecoveryEffect::ConvertBoth { flight: 1 }], "the reloaded Engine had no picture");
        let mut m = manual_machine();
        run(&mut m, RecoveryEvent::FirstPicture);
        let with_picture = OpenFacts { no_frames: false, ..manual_on("ac3", h264_1080p()) };
        let (_, fx) = run(&mut m, refused(cause, with_picture));
        assert_eq!(fx, [RecoveryEffect::ShowError { refusal: cause }]);
    }

    /// The both rung's own refused dispatch is terminal: the read-out.
    #[test]
    fn a_refused_both_facets_flight_reads_out() {
        let mut m = manual_machine();
        let cause = LoadRefusal::Refused { num: 601 };
        run(&mut m, refused(cause, manual_on("ac3", h264_1080p())));
        assert_eq!(m.phase(), Phase::Flight);
        let (_, fx) = run(&mut m, RecoveryEvent::FlightRefused);
        assert_eq!((fx, m.phase()), (vec![RecoveryEffect::ShowError { refusal: cause }], Phase::Failed));
    }

    /// A timeout or a source failure names no pipeline stream: the manual read-out, as on main.
    #[test]
    fn a_timeout_or_source_failure_never_takes_the_manual_both_rung() {
        for other in [LoadRefusal::TimedOut, LoadRefusal::SourceFailed] {
            let (_, fx) = run(&mut manual_machine(), refused(other, on_4k("ac3", false)));
            assert_eq!(fx, [RecoveryEffect::ShowError { refusal: other }], "{other:?}");
        }
    }

    /// A video whose raster the server never measured (0x0) names nothing, as the direct-play gate
    /// passes one it was never told: it does not earn the manual both-facets rung either.
    #[test]
    fn an_unmeasured_video_raster_never_earns_the_manual_both_rung() {
        let unknown = Some(StreamVideo { codec: "hevc".into(), width: 0, height: 0 });
        let (_, fx) = run(&mut manual_machine(), refused(SHUT, manual_on("ac3", unknown)));
        assert_eq!(fx, ERROR);
    }

    /// A cold Auto Original's refused HLS rescue is not converted again: the HLS route feeds AAC and
    /// an encoded video, and a landed HLS reload reads `auto_original == false` (progressive MKV is
    /// required of the watch), so the manual rung's own clause is what stops it.
    #[test]
    fn a_refused_hls_rescue_is_not_converted_again() {
        let mut m = manual_machine();
        let cause = LoadRefusal::Refused { num: 100 };
        let (_, fx) = run(&mut m, refused(cause, on_4k("ac3", true)));
        assert_eq!(fx, [RecoveryEffect::StartAutoHls]);
        run(&mut m, landed());
        let (_, fx) = run(&mut m, refused(LoadRefusal::Refused { num: 601 }, manual_on("aac", None)));
        assert_eq!((fx, m.phase()), (vec![RecoveryEffect::ShowError { refusal: LoadRefusal::Refused { num: 601 } }], Phase::Failed));
    }

    /// A cold Auto Original keeps its HLS contingency as the video re-encode: the refusal goes to
    /// `StartAutoHls` with the refused raster out of the HLS attempt.
    #[test]
    fn a_cold_auto_original_takes_hls_for_a_video_refusal_with_the_raster_lowered() {
        let mut m = manual_machine();
        let (_, fx) = run(&mut m, refused(LoadRefusal::Refused { num: 100 }, on_4k("ac3", true)));
        assert_eq!(fx, [RecoveryEffect::StartAutoHls]);
        assert_eq!(m.attempt().raster_bound("h264"), (1920, 1088));
        // A timeout names no stream: main's attempt flies.
        let mut timed_out = manual_machine();
        run(&mut timed_out, refused(LoadRefusal::TimedOut, on_4k("ac3", true)));
        assert_eq!(timed_out.attempt().raster_bound("h264"), (3840, 2176));
    }

    /// A video facet flight never proves (D6(c)): its landing re-enters `Opening` even on the
    /// verdicts that would prove an audio facet, and the first picture then credits nothing.
    #[test]
    fn a_video_facet_flight_never_proves() {
        let mut m = manual_machine();
        run(&mut m, refused(LoadRefusal::Refused { num: 100 }, on_4k("ac3", false)));
        run(&mut m, landed_with(Lanes { video: Lane::Transcode, audio: Lane::Copy }));
        assert_eq!(m.phase(), Phase::Opening);
        let (_, fx) = run_proof(&mut m, RecoveryEvent::FirstPicture);
        assert!(fx.is_empty(), "{fx:?}");
    }

    /// A refused video flight ends on the read-out: the both rung would fly the identical encode.
    #[test]
    fn a_refused_video_facet_flight_is_not_flown_again() {
        let mut m = manual_machine();
        let cause = LoadRefusal::Refused { num: 100 };
        run(&mut m, refused(cause, on_4k("ac3", false)));
        let (_, fx) = run(&mut m, RecoveryEvent::FlightRefused);
        assert_eq!(fx, [RecoveryEffect::ShowError { refusal: cause }]);
        for other in [LoadRefusal::TimedOut, LoadRefusal::SourceFailed] {
            let (_, fx) = run(&mut manual_machine(), refused(other, on_4k("ac3", false)));
            assert_eq!(fx, [RecoveryEffect::ShowError { refusal: other }], "{other:?}");
        }
    }

    /// **A Load whose audio the app decoded names no native audio facet.** Where the attempt decodes it in the app the
    /// pipeline was fed PCM, so a refusal says nothing about its own DTS decoder; naming `dts` would
    /// convert it, and a proven conversion would be STORED as a refusal of the set's native DTS
    /// (`limit_codec` caps it at two channels), which outlives the app decode.
    #[test]
    fn an_app_decoded_lossy_load_names_no_audio_facet() {
        assert_eq!(suspect(audio("dca", 6).as_ref(), None), Some(dts6()), "the native Load still does");
        assert_eq!(suspect(app_decoded("dca", 6).as_ref(), None), None);
        assert_eq!(suspect(app_decoded("dca", 6).as_ref(), h264_4k().as_ref()), Some(h264_4k_facet()),
            "elimination: the video is then the suspect");
        let fed = |auto, no_frames| OpenFacts { audio: app_decoded("dca", 6), ..facts(false, auto, no_frames) };
        let mut m = machine();
        let (_, fx) = run(&mut m, refused(SHUT, fed(false, true)));
        assert_eq!(fx, ERROR, "a manual pick: nothing to convert, the error shows");
        assert_eq!(m.attempt().audio_supports("dts", 6), true, "native DTS is not narrowed");
        let mut m = machine();
        let (_, fx) = run(&mut m, refused(NATIVE, fed(true, true)));
        assert_eq!(fx, [RecoveryEffect::StartAutoHls], "Auto Original falls to its HLS contingency");
        assert_eq!(m.attempt().audio_supports("dts", 6), true, "and the HLS attempt keeps native DTS");
    }

    #[test]
    fn suspect_names_audio_outside_aac_ac3_eac3_only() {
        let s = |codec: &str, channels| suspect(audio(codec, channels).as_ref(), None);
        assert_eq!(s("dca", 6), Some(dts6()));
        assert_eq!(s("opus", 6), Some(opus6()));
        for known in ["aac", "ac3", "eac3"] {
            assert_eq!(s(known, 6), None, "{known}");
        }
        assert_eq!(suspect(None, None), None);
        assert_eq!(s("dca", 0), Some(Facet::Audio { codec: "dts".into(), channels: 0 }), "unknown width names the codec");
    }

    #[test]
    fn a_pipeline_refusal_with_an_audio_suspect_converts_that_facet_once() {
        let start = dts_start();
        assert_ne!(start.without(&dts6()), *start, "the fixture must exercise a real narrowing");
        let mut m = OpenRecovery::new(start.clone());
        let (handled, fx) = run(&mut m, refused(SHUT, on_dts(facts(false, false, true))));
        assert_eq!((handled, fx, m.phase()), (Handled::Yes, vec![RecoveryEffect::ConvertFacet { facet: dts6(), flight: 1 }], Phase::Flight));
        assert_eq!(*m.attempt(), start.without(&dts6()));
    }

    #[test]
    fn an_auto_original_with_an_audio_suspect_converts_before_hls() {
        let start = dts_start();
        assert_ne!(start.without(&dts6()), *start);
        let mut m = OpenRecovery::new(start.clone());
        let (_, fx) = run(&mut m, refused(SHUT, on_dts(facts(false, true, true))));
        assert_eq!(fx, [RecoveryEffect::ConvertFacet { facet: dts6(), flight: 1 }], "the conversion keeps the Original route; HLS would throw it away");
        assert_eq!(*m.attempt(), start.without(&dts6()));
    }

    #[test]
    fn a_refusal_after_the_facet_flight_takes_the_both_facets_rung_then_the_read_out() {
        let start = dts_start();
        assert_ne!(start.without(&dts6()), *start);
        // Cold Auto Original: the facet, then the both-facets rung (HLS), then the read-out.
        let mut m = OpenRecovery::new(start.clone());
        run(&mut m, refused(SHUT, on_dts(facts(false, true, true))));
        run(&mut m, landed());
        // The converted route feeds AAC now: no suspect, so only the HLS rung is left.
        let converted = OpenFacts { audio: audio("aac", 2), ..facts(false, true, true) };
        let (_, fx) = run(&mut m, refused(SHUT, converted.clone()));
        assert_eq!((fx, m.phase()), (vec![RecoveryEffect::StartAutoHls], Phase::Flight));
        assert_eq!(*m.attempt(), start.without(&dts6()), "derived from start, the facet carried");
        run(&mut m, landed());
        let (_, fx) = run(&mut m, refused(SHUT, converted));
        assert_eq!((fx, m.phase()), (ERROR.to_vec(), Phase::Failed));
        // Manual Original: the facet, then the both-facets rung (an encode with no video copy), then
        // the read-out. The converted remux still feeds the source's video, so the video is a
        // candidate for the encode; the encode's own refusal feeds an encoded video: nothing is left.
        let mut m = OpenRecovery::new(start.clone());
        run(&mut m, refused(SHUT, on_dts(facts(false, false, true))));
        run(&mut m, landed());
        let remux = OpenFacts { audio: audio("aac", 2), video: h264_1080p(), ..facts(false, false, true) };
        let (_, fx) = run(&mut m, refused(SHUT, remux));
        assert_eq!((fx, m.phase()), (vec![RecoveryEffect::ConvertBoth { flight: 2 }], Phase::Flight));
        assert_eq!(*m.attempt(), start.without(&dts6()), "nothing of the 1080p video to lower");
        run(&mut m, landed());
        let encoded = OpenFacts { audio: audio("aac", 2), ..facts(false, false, true) };
        let (_, fx) = run(&mut m, refused(SHUT, encoded));
        assert_eq!((fx, m.phase()), (ERROR.to_vec(), Phase::Failed));
        // A converted route whose video is unknown or encoded has no both rung: the read-out.
        let mut m = OpenRecovery::new(start);
        run(&mut m, refused(SHUT, on_dts(facts(false, false, true))));
        run(&mut m, landed());
        let (_, fx) = run(&mut m, refused(SHUT, OpenFacts { audio: audio("aac", 2), ..facts(false, false, true) }));
        assert_eq!((fx, m.phase()), (ERROR.to_vec(), Phase::Failed));
    }

    /// **M1. The app decode is the LAST rung** of a direct play on a set the policy admits DTS on: the
    /// facet rung (the server converts the audio) is refused, the HLS contingency of a cold Auto Original
    /// is refused, and only then does the app decode, once, with the attempt announcing the codec.
    #[test]
    fn the_app_decode_rung_is_last_after_a_refused_conversion_on_a_direct_play() {
        let mut m = measured_machine();
        let first = direct(on_dts(facts(false, true, true)));
        assert_eq!(run(&mut m, refused(SHUT, first)).1, [RecoveryEffect::ConvertFacet { facet: dts6(), flight: 1 }]);
        let (_, fx) = run(&mut m, RecoveryEvent::FlightRefused);
        assert_eq!((fx, m.phase()), (vec![RecoveryEffect::StartAutoHls], Phase::Flight), "HLS before the app");
        let (handled, fx) = run(&mut m, RecoveryEvent::FlightRefused);
        assert_eq!((handled, fx, m.phase()), (Handled::Yes, vec![decode_dts(SHUT)], Phase::Opening));
        let attempt = m.attempt();
        assert!(attempt.app_lossy_in_force.contains("dts"), "in force: the app decodes it");
        assert_eq!(attempt.audio_channels.get("dts"), Some(&8), "and announces it whatever the facet took out");
        assert!(!attempt.native_audio_track("dts", 6, 0, false));
        assert!(attempt.app_decodes_lossy_track("dts", 6, 0, false));
        assert_eq!(m.flights, 2, "the rung is not a flight");
        // A manual pick has no HLS rung: its refused facet flight goes straight to the app.
        let mut m = measured_machine();
        run(&mut m, refused(SHUT, direct(on_dts(facts(false, false, true)))));
        let (_, fx) = run(&mut m, RecoveryEvent::FlightRefused);
        assert_eq!((fx, m.phase()), (vec![decode_dts(SHUT)], Phase::Opening));
        assert_eq!(m.flights, 1);
    }

    /// **M1b. A surround track is decoded in the app LAST, after the manual encode** (owner decision
    /// 2026-10-28: the app never mixes a source of more than two channels down to stereo by default). The
    /// facet rung (the server converts the audio, keeping the channels) is refused, and the next rung is the
    /// both-facets encode (`ConvertBoth`), not the app decode; only when the encode is refused too does the
    /// app decode, once, with the attempt announcing the codec.
    #[test]
    fn a_surround_track_is_decoded_in_the_app_only_after_the_manual_encode_is_refused() {
        let mut m = measured_machine();
        let first = direct(OpenFacts { video: h264_1080p(), ..on_dts(facts(false, false, true)) });
        assert_eq!(run(&mut m, refused(SHUT, first)).1, [RecoveryEffect::ConvertFacet { facet: dts6(), flight: 1 }]);
        let (_, fx) = run(&mut m, RecoveryEvent::FlightRefused);
        assert_eq!((fx, m.phase()), (vec![RecoveryEffect::ConvertBoth { flight: 2 }], Phase::Flight), "the encode before the app");
        assert!(m.in_app.is_none(), "the app has not decoded anything yet");
        let (handled, fx) = run(&mut m, RecoveryEvent::FlightRefused);
        assert_eq!((handled, fx, m.phase()), (Handled::Yes, vec![decode_dts(SHUT)], Phase::Opening), "the last rung");
        assert!(m.attempt().app_lossy_in_force.contains("dts"), "in force: the app decodes it");
        assert_eq!(m.flights, 2, "the rung is not a flight");
        // The app-decoded Load refused as well: the read-out (the rung is spent, the flights are too).
        let app_load = direct(OpenFacts { audio: app_decoded("dca", 6), video: h264_1080p(), ..facts(false, false, true) });
        let (_, fx) = run(&mut m, refused(SHUT, app_load));
        assert_eq!((fx, m.phase()), (ERROR.to_vec(), Phase::Failed));
        // With no video fed there is no encode to try: the app decode is the only rung left, and it runs.
        let mut m = measured_machine();
        run(&mut m, refused(SHUT, direct(on_dts(facts(false, false, true)))));
        let (_, fx) = run(&mut m, RecoveryEvent::FlightRefused);
        assert_eq!(fx, [decode_dts(SHUT)], "nothing server-side is left to try first");
        // A track whose width is unknown (0) is as likely a surround source: it takes the encode first too.
        let mut m = measured_machine();
        let unknown = direct(OpenFacts { video: h264_1080p(), ..on_dts_ch(facts(false, false, true), 0) });
        run(&mut m, refused(SHUT, unknown));
        let (_, fx) = run(&mut m, RecoveryEvent::FlightRefused);
        assert_eq!(fx, [RecoveryEffect::ConvertBoth { flight: 2 }], "an unknown width is not proven stereo");
    }

    /// **M1c. A stereo (or mono) track keeps its place before the encode**: the app decode loses nothing
    /// there and keeps the direct play, so it comes right after the refused facet flight, and the encode is
    /// the rung after it (today's order).
    #[test]
    fn a_stereo_track_is_decoded_in_the_app_before_the_manual_encode() {
        for channels in [1, 2] {
            let mut m = measured_machine();
            let first = direct(OpenFacts { video: h264_1080p(), ..on_dts_ch(facts(false, false, true), channels) });
            let facet = Facet::Audio { codec: "dts".into(), channels };
            assert_eq!(run(&mut m, refused(SHUT, first)).1, [RecoveryEffect::ConvertFacet { facet, flight: 1 }], "{channels}ch");
            let (_, fx) = run(&mut m, RecoveryEvent::FlightRefused);
            assert_eq!((fx, m.phase()), (vec![decode_dts_ch(SHUT, channels)], Phase::Opening), "{channels}ch: the app before the encode");
            assert_eq!(m.flights, 1);
            // The app-decoded Load refused: now the encode, then the read-out.
            let app_load = direct(OpenFacts { audio: app_decoded("dca", channels), video: h264_1080p(), ..facts(false, false, true) });
            let (_, fx) = run(&mut m, refused(SHUT, app_load.clone()));
            assert_eq!((fx, m.phase()), (vec![RecoveryEffect::ConvertBoth { flight: 2 }], Phase::Flight), "{channels}ch");
            let (_, fx) = run(&mut m, RecoveryEvent::FlightRefused);
            assert_eq!((fx, m.phase()), (ERROR.to_vec(), Phase::Failed), "{channels}ch");
        }
    }

    /// **M1d. Force Direct Play never mixes a surround track down.** Where the set has no native path for
    /// a track of more than two channels (or of an unknown width), Forced reads the refusal out; the
    /// stereo track is decoded as ever. The `appdecodelossy` debug trigger (`AppLossy::All`) is the one
    /// explicit override.
    #[test]
    fn forced_never_mixes_a_surround_track_down_in_the_app() {
        let forced = |channels| OpenFacts { forced: true, ..direct(on_dts_ch(facts(false, false, true), channels)) };
        for channels in [3, 6, 8, 0] {
            let mut m = measured_machine();
            let (handled, fx) = run(&mut m, refused(SHUT, forced(channels)));
            assert_eq!((handled, fx, m.phase()), (Handled::Yes, ERROR.to_vec(), Phase::Failed), "{channels}ch: the refusal reads out; a DTS row plays DTS, so the audio is not named");
            assert!(m.in_app.is_none() && !m.attempt().app_lossy_in_force.contains("dts"), "{channels}ch: nothing decodes in the app");
            assert_eq!(m.flights, 0, "{channels}ch: Forced converts nothing");
        }
        // A set with no DTS row does not play it, so the refusal reads out naming the codec.
        let no_row = || OpenRecovery::new(Arc::new(Caps { app_lossy: plx_platform::devcaps::AppLossy::Measured, ..Caps::assumed() }));
        for channels in [3, 6, 8, 0] {
            let (_, fx) = run(&mut no_row(), refused(SHUT, forced(channels)));
            assert_eq!(fx, forced_dts_error(), "{channels}ch: no DTS row: the codec is named");
        }
        for channels in [1, 2] {
            let (_, fx) = run(&mut measured_machine(), refused(SHUT, forced(channels)));
            assert_eq!(fx, [decode_dts_ch(SHUT, channels)], "{channels}ch: not a downmix");
        }
        let all = OpenRecovery::new(Arc::new(Caps { app_lossy: plx_platform::devcaps::AppLossy::All, ..(*dts_start()).clone() }));
        let (_, fx) = run(&mut all.clone(), refused(SHUT, forced(6)));
        assert_eq!(fx, [decode_dts(SHUT)], "the debug trigger is the explicit override");
    }

    /// **M2. A cold Auto Original reaches the rung after its HLS contingency is refused too**, the refused
    /// video raster the HLS attempt took out is given back (the reload feeds the same source video, so a
    /// lowered raster ceiling would shrink the Load's video envelope for nothing), and the codec is in force.
    #[test]
    fn a_cold_auto_original_decodes_in_the_app_after_hls_is_refused() {
        let mut m = measured_machine();
        let facts_4k = direct(OpenFacts { video: h264_4k(), ..on_dts(facts(false, true, true)) });
        run(&mut m, refused(SHUT, facts_4k));
        run(&mut m, RecoveryEvent::FlightRefused);
        assert_eq!(m.phase(), Phase::Flight, "the HLS rung is flying");
        assert!(m.excluded.video.is_some(), "its attempt lacks the refused raster");
        assert!(!m.attempt().video_ceiling.is_empty());
        let (_, fx) = run(&mut m, RecoveryEvent::FlightRefused);
        assert_eq!(fx, [decode_dts(SHUT)]);
        assert!(m.excluded.video.is_none(), "the raster comes back");
        assert!(m.attempt().video_ceiling.is_empty(), "the Load's video envelope is the source's again");
        assert!(m.attempt().app_lossy_in_force.contains("dts"));
        assert_eq!(m.attempt().audio_channels.get("dts"), Some(&8));
    }

    /// **M3. Forced decodes a refused STEREO lossy track in the app and does nothing else**: no conversion,
    /// no flight. Everything the rung needs, and nothing it does not: a codec the policy has not enabled, a
    /// failure that is not the pipeline's, a route with a server in it, an `Off` start, a stream the
    /// app already decodes, a Load that showed a picture. (A surround track is not decoded at all under
    /// Force: `forced_never_mixes_a_surround_track_down_in_the_app`.)
    #[test]
    fn forced_decodes_a_refused_lossy_track_in_the_app_and_nothing_else() {
        let forced = |f: OpenFacts| OpenFacts { forced: true, ..f };
        let base = || forced(direct(on_dts_ch(facts(false, false, true), 2)));
        for refusal in [SHUT, NATIVE, LoadRefusal::Stalled] {
            let mut m = measured_machine();
            let (handled, fx) = run(&mut m, refused(refusal, base()));
            assert_eq!((handled, fx, m.phase()), (Handled::Yes, vec![decode_dts_ch(refusal, 2)], Phase::Opening), "{refusal}");
            assert_eq!(m.flights, 0, "no flight: Forced asks no server");
            assert!(m.attempt().app_lossy_in_force.contains("dts"));
        }
        // The read-out names the codec of a native audio the TV refused as the play started (`named`).
        let ends_as = |m: &mut OpenRecovery, refusal: LoadRefusal, f: OpenFacts, named: bool, what: &str| {
            let codec = f.audio.as_ref().map(|a| a.codec.clone()).unwrap_or_default();
            let (_, fx) = run(m, refused(refusal, f));
            let want: Vec<_> = named.then(|| RecoveryEffect::NameRefusedAudio { codec }).into_iter()
                .chain([RecoveryEffect::ShowError { refusal }]).collect();
            assert_eq!((fx, m.phase()), (want, Phase::Failed), "{what}");
        };
        let ends = |m: &mut OpenRecovery, refusal: LoadRefusal, f: OpenFacts, what: &str| ends_as(m, refusal, f, false, what);
        ends(&mut measured_machine(), SHUT, OpenFacts { audio: audio("ac3", 6), ..base() }, "ac3: no row to enable");
        let (_, fx) = run(&mut measured_machine(), refused(SHUT, OpenFacts { audio: audio("truehd", 8), ..base() }));
        assert_eq!(
            fx,
            [RecoveryEffect::NameRefusedAudio { codec: "truehd".into() }, RecoveryEffect::ShowError { refusal: SHUT }],
            "truehd: unmeasured, not played, and the codec is named"
        );
        ends(&mut measured_machine(), LoadRefusal::TimedOut, base(), "a timeout names no pipeline refusal");
        ends(&mut measured_machine(), LoadRefusal::SourceFailed, base(), "a source failure is not the pipeline's");
        ends(&mut measured_machine(), SHUT, OpenFacts { direct: false, ..base() }, "a server session in the way; a DTS row plays it");
        ends_as(&mut OpenRecovery::new(Arc::new(Caps::assumed())), SHUT, OpenFacts { direct: false, ..base() }, true, "a server session in the way, no DTS row");
        ends_as(&mut OpenRecovery::new(Arc::new(Caps::assumed())), SHUT, base(), true, "an Off start with no DTS row");
        ends(&mut measured_machine(), SHUT, OpenFacts { audio: app_decoded("dca", 6), ..base() }, "already decoded in the app");
        ends(&mut measured_machine(), SHUT, OpenFacts { no_frames: false, ..base() }, "a picture was already shown");
        ends(&mut measured_machine(), SHUT, OpenFacts { audio: None, ..base() }, "no audio fed");
        // Not Forced, with a picture: the facet rung and the app are both for an Engine with none.
        let mut m = measured_machine();
        let (_, fx) = run(&mut m, refused(SHUT, direct(on_dts(facts(false, false, false)))));
        assert_eq!(fx, ERROR);
    }

    /// **M4. The rung runs once per request.** A second pipeline refusal after it (the app-decoded Load
    /// refused, or a reload that fed the codec natively again) is the read-out, whatever the facts say.
    #[test]
    fn the_app_decode_rung_runs_once_per_request() {
        let forced = |f: OpenFacts| OpenFacts { forced: true, ..f };
        // The native DTS the TV refuses again is named; an app-decoded Load's refusal names no audio.
        for (second, want) in [
            (on_dts_ch(facts(false, false, true), 2), forced_dts_error()),
            (OpenFacts { audio: app_decoded("dca", 2), ..facts(false, false, true) }, ERROR.to_vec()),
        ] {
            let mut m = measured_machine();
            run(&mut m, refused(SHUT, forced(direct(on_dts_ch(facts(false, false, true), 2)))));
            assert_eq!(m.phase(), Phase::Opening);
            let (handled, fx) = run(&mut m, refused(SHUT, forced(direct(second))));
            assert_eq!((handled, fx, m.phase()), (Handled::Yes, want, Phase::Failed));
        }
        // Not Forced: after the rung, the app-decoded Load's refusal names no audio and no video that
        // is still to lower, so the read-out follows.
        let mut m = measured_machine();
        run(&mut m, refused(SHUT, direct(on_dts(facts(false, false, true)))));
        run(&mut m, RecoveryEvent::FlightRefused);
        assert_eq!(m.phase(), Phase::Opening);
        let (_, fx) = run(&mut m, refused(SHUT, direct(OpenFacts { audio: app_decoded("dca", 6), ..facts(false, false, true) })));
        assert_eq!(fx, ERROR);
        // And a Load nobody asked for after the read-out reopens the ladder without the app decode: the
        // native DTS that refuses again is the manual encode's to convert (the one flight a manual pick
        // has left), never the rung's.
        run(&mut m, RecoveryEvent::LoadStarted);
        let (_, fx) = run(&mut m, refused(SHUT, direct(on_dts(facts(false, false, true)))));
        assert_eq!(fx, [RecoveryEffect::ConvertBoth { flight: 2 }], "the rung stays spent");
    }

    /// **M5. An app decode never proves, and neither does a flight after one.** The reload the rung asked
    /// for is an `Opening` with no flight to land, so its first picture credits nothing. And a facet
    /// flight flown on the strength of a refused APP-DECODED Load (no audio suspect, so the video is)
    /// must not credit the audio facet an earlier rung left in the request: the proof would store a
    /// refusal of the set's own decoder that the Load never exercised.
    #[test]
    fn an_app_decode_never_proves_and_an_app_decoded_refusal_never_proves() {
        let mut m = measured_machine();
        run(&mut m, refused(SHUT, OpenFacts { forced: true, ..direct(on_dts_ch(facts(false, false, true), 2)) }));
        assert_eq!(run(&mut m, landed_with(CLEAN)), (Handled::No, vec![]), "no flight is outstanding");
        assert_eq!(run(&mut m, RecoveryEvent::FirstPicture), (Handled::Yes, vec![]), "and its picture proves nothing");
        assert_eq!(m.phase(), Phase::Playing);

        // A rollback that named native DTS left the audio facet in the request; then the app-decoded Load
        // is refused, the video is the suspect, and the video flight lands "clean".
        let mut m = measured_machine();
        let pending = OpenFacts { pending_original: true, native_audio: Some(dts6()), ..on_dts(facts(true, false, true)) };
        let (_, fx) = run(&mut m, refused(NATIVE, pending));
        assert_eq!(fx, [named(dts6()), RecoveryEffect::Rollback]);
        run(&mut m, landed());
        let app_load = direct(OpenFacts { video: h264_4k(), audio: app_decoded("dca", 6), ..facts(false, false, true) });
        let (_, fx) = run(&mut m, refused(SHUT, app_load));
        assert_eq!(fx, [RecoveryEffect::ConvertFacet { facet: h264_4k_facet(), flight: 1 }]);
        run(&mut m, landed_with(CLEAN));
        assert_eq!(m.phase(), Phase::Opening, "no pending proof");
        let (handled, fx) = run(&mut m, RecoveryEvent::FirstPicture);
        assert_eq!(handled, Handled::Yes);
        assert!(!fx.iter().any(|e| matches!(e, RecoveryEffect::ConfirmRefusal { .. })), "so no write at the picture: {fx:?}");
        assert!(
            matches!(fx.as_slice(), [RecoveryEffect::ReportRecovered(r)] if r.rung == Rung::Facet),
            "the flight did rescue the open, which the report says (and only the report): {fx:?}"
        );
    }

    /// **M7. The rung's pairs**: where the app decode is asked for, from which state and event, and what
    /// is left of the request afterwards. (`every_state_and_event_pair_has_a_pinned_outcome` pins the
    /// start with no policy.)
    #[test]
    fn the_app_decode_rung_has_a_pinned_outcome_in_every_state_and_event() {
        use Handled::{No, Yes};
        let manual = || direct(on_dts(facts(false, false, true)));
        let cold = || direct(on_dts(facts(false, true, true)));
        let at = |phase: Phase| {
            let mut m = measured_machine();
            match phase {
                Phase::Opening => {}
                Phase::Playing => {
                    run(&mut m, RecoveryEvent::FirstPicture);
                }
                Phase::Flight => {
                    run(&mut m, refused(SHUT, manual()));
                }
                Phase::Failed => {
                    run(&mut m, refused(SHUT, OpenFacts { audio: audio("ac3", 6), ..manual() }));
                }
                Phase::Proving => unreachable!(),
            }
            assert_eq!(m.phase(), phase, "the fixture");
            m
        };
        // Opening and Playing rule a refusal alike, and a manual pick's facet rung comes first.
        for phase in [Phase::Opening, Phase::Playing] {
            let (handled, fx) = run(&mut at(phase), refused(SHUT, manual()));
            assert_eq!((handled, fx), (Yes, vec![RecoveryEffect::ConvertFacet { facet: dts6(), flight: 1 }]), "{phase:?}");
        }
        // A facet flight refused: the rung; a landing: the reload re-enters the open, nothing asked.
        let (handled, fx) = run(&mut at(Phase::Flight), RecoveryEvent::FlightRefused);
        assert_eq!((handled, fx), (Yes, vec![decode_dts(SHUT)]));
        assert_eq!(run(&mut at(Phase::Flight), landed()), (Yes, vec![]));
        // The HLS contingency refused: the rung. A rollback refused stays terminal.
        let mut m = measured_machine();
        run(&mut m, refused(SHUT, cold()));
        run(&mut m, RecoveryEvent::FlightRefused);
        assert_eq!(run(&mut m, RecoveryEvent::FlightRefused), (Yes, vec![decode_dts(SHUT)]));
        let mut m = measured_machine();
        let pending = OpenFacts { pending_original: true, ..cold() };
        assert_eq!(run(&mut m, refused(SHUT, pending)).1, [RecoveryEffect::Rollback]);
        assert_eq!(run(&mut m, RecoveryEvent::FlightRefused), (Yes, ERROR.to_vec()), "a refused rollback is terminal");
        assert_eq!(m.phase(), Phase::Failed);
        // After the rung the machine is an ordinary `Opening`: its picture and a foreign Load are as ever.
        let mut m = measured_machine();
        run(&mut m, refused(SHUT, OpenFacts { forced: true, ..direct(on_dts_ch(facts(false, false, true), 2)) }));
        assert_eq!(run(&mut m, RecoveryEvent::LoadStarted), (No, vec![]), "the rung's own reload changes nothing");
        assert_eq!(run(&mut m, RecoveryEvent::FlightRefused), (No, vec![]));
        assert_eq!(run(&mut m, RecoveryEvent::FirstPicture), (Yes, vec![]));
        assert_eq!(m.phase(), Phase::Playing);
        // The read-out is not left by the rung.
        let mut m = at(Phase::Failed);
        assert_eq!(run(&mut m, refused(SHUT, manual())), (No, vec![]));
    }

    /// **Every reachable state of one request**, not a depth of paths: the walk runs to a fixpoint
    /// over (state, flights, excluded facets), which is everything the machine's future depends on,
    /// from every event the driver can raise (a refusal of an Engine with and without a picture, a
    /// landing with and without a clean verdict, the first picture, a Load nobody asked for). On
    /// every path the flights a request starts never exceed the cap, and a facet conversion is only
    /// ever the request's first flight. `Playing` is inside the proof: the first picture is an event.
    #[test]
    fn at_most_two_flights_follow_one_request() {
        // (cold Auto Original, the audio fed, the video fed): the audio-suspect profiles of S1a and the
        // video ones (a 4K H.264 behind an audio no set refuses, and behind one that is a suspect).
        let profiles = [(true, "dca", None), (false, "dca", None), (true, "ac3", h264_4k()), (false, "ac3", h264_4k()), (true, "dca", h264_4k()), (false, "dca", h264_4k()), (false, "ac3", h264_1080p()), (true, "ac3", h264_1080p())];
        // Every profile on a start with no policy and on one the policy admits DTS on, for a direct play
        // and Forced alike: the app-decode rung (`in_app`) and `unprovable` are two more dimensions. And
        // for a stereo and a surround source alike: the width decides the rung's place (and, under Force,
        // whether it exists at all).
        let shapes = [(false, false), (true, false), (true, true)];
        let widths = [2, 6];
        for (((auto, fed, video), (measured, forced)), channels) in profiles
            .into_iter()
            .flat_map(|p| shapes.into_iter().map(move |s| (p.clone(), s)))
            .flat_map(|ps| widths.into_iter().map(move |w| (ps.clone(), w)))
        {
            let base = OpenFacts { auto_original: auto, no_frames: true, forced, direct: measured, audio: audio(fed, channels), video, ..OpenFacts::default() };
            let new_machine = || if measured { measured_machine() } else { machine() };
            let stall = OpenFacts { native_audio: Some(opus6()), ..base.clone() };
            let with_picture = OpenFacts { no_frames: false, ..base.clone() };
            let events = [
                refused(SHUT, base.clone()),
                refused(SHUT, with_picture),
                refused(LoadRefusal::TimedOut, base.clone()),
                refused(LoadRefusal::Stalled, stall),
                refused(LoadRefusal::SourceFailed, base),
                landed(),
                landed_with(CLEAN),
                RecoveryEvent::FlightRefused,
                RecoveryEvent::FirstPicture,
                RecoveryEvent::LoadStarted,
            ];
            let key = |m: &OpenRecovery| format!("{:?}|{}|{:?}|{}|{:?}|{}", m.state, m.flights, m.excluded, m.pictured, m.in_app, m.unprovable);
            let mut seen = std::collections::HashSet::from([key(&new_machine())]);
            let mut frontier = vec![new_machine()];
            while let Some(m) = frontier.pop() {
                for ev in &events {
                    let mut next = m.clone();
                    let mut fx = Vec::new();
                    next.step(ev, &mut fx);
                    let started = fx.iter().filter(|e| matches!(e, RecoveryEffect::ConvertFacet { .. } | RecoveryEffect::ConvertBoth { .. } | RecoveryEffect::StartAutoHls)).count();
                    if fx.iter().any(|e| matches!(e, RecoveryEffect::ConvertFacet { .. })) {
                        assert_eq!(m.flights, 0, "a facet conversion after another flight: {ev:?} in {:?}", m.state);
                    }
                    assert_eq!(next.flights as usize, m.flights as usize + started, "flights count what started: {ev:?}");
                    assert!(next.flights <= MAX_RECOVERY_FLIGHTS, "{} flights started on one request", next.flights);
                    // The rung is no flight, runs once per request, and only on a start the policy admits.
                    let rung = fx.iter().filter(|e| matches!(e, RecoveryEffect::DecodeInApp { .. })).count();
                    assert!(rung <= 1, "{fx:?}");
                    if rung == 1 {
                        assert!(measured && m.in_app.is_none() && next.in_app.is_some(), "{ev:?} in {:?}", m.state);
                        // Force Direct Play never mixes a surround source down.
                        assert!(!forced || channels <= 2, "a Forced {channels}-channel track was decoded in the app: {ev:?}");
                        assert!(next.excluded.video.is_none(), "the video raster comes back");
                    }
                    assert!(m.in_app.is_none() || next.in_app == m.in_app, "the rung is spent for good");
                    assert!(measured || next.in_app.is_none(), "no policy, no rung");
                    if seen.insert(key(&next)) {
                        frontier.push(next);
                    }
                }
            }
            // Forced converts nothing, so it has few states: the rung's (a stereo track only), and the read-out.
            let floor = if forced && channels > 2 { 2 } else if forced { 3 } else { 5 };
            assert!(seen.len() > floor, "the walk reached {} states", seen.len());
        }
    }

    /// **Every state x event pair has a pinned outcome**: whether it is handled, where it goes, and
    /// what it asks for. An unhandled pair changes nothing and asks for nothing. (`Refused` is a DTS
    /// 5.1 refusal of an Engine with no picture on a manual Original; `Proving` is entered by a
    /// clean facet landing.)
    #[test]
    fn every_state_and_event_pair_has_a_pinned_outcome() {
        use Handled::{No, Yes};
        use Phase::{Failed, Flight, Opening, Playing, Proving};
        let refusal = || refused(SHUT, on_dts(facts(false, false, true)));
        let named_refusal = || refused(NATIVE, OpenFacts { native_audio: Some(dts6()), ..on_dts(facts(false, false, true)) });
        let convert = || vec![RecoveryEffect::ConvertFacet { facet: dts6(), flight: 1 }];
        // (state, event name, event, handled, next state, effects)
        #[allow(clippy::type_complexity)]
        let table: Vec<(Phase, &str, RecoveryEvent, Handled, Phase, Vec<RecoveryEffect>)> = vec![
            (Opening, "Refused", refusal(), Yes, Flight, convert()),
            (Opening, "FlightLanded", landed(), No, Opening, vec![]),
            (Opening, "FlightRefused", RecoveryEvent::FlightRefused, No, Opening, vec![]),
            (Opening, "FirstPicture", RecoveryEvent::FirstPicture, Yes, Playing, vec![]),
            (Opening, "LoadStarted", RecoveryEvent::LoadStarted, No, Opening, vec![]),
            (Proving, "Refused", refusal(), Yes, Failed, ERROR.to_vec()),
            (Proving, "FlightLanded", landed_with(CLEAN), No, Proving, vec![]),
            (Proving, "FlightRefused", RecoveryEvent::FlightRefused, No, Proving, vec![]),
            (Proving, "FirstPicture", RecoveryEvent::FirstPicture, Yes, Playing, vec![proof(dts6()), recovered(Rung::Facet, SHUT, Some(dts6()))]),
            (Proving, "LoadStarted", RecoveryEvent::LoadStarted, Yes, Proving, vec![]),
            (Flight, "Refused", refusal(), No, Flight, vec![]),
            (Flight, "FlightLanded", landed(), Yes, Opening, vec![]),
            (Flight, "FlightRefused", RecoveryEvent::FlightRefused, Yes, Failed, ERROR.to_vec()),
            (Flight, "FirstPicture", RecoveryEvent::FirstPicture, No, Flight, vec![]),
            (Flight, "LoadStarted", RecoveryEvent::LoadStarted, Yes, Opening, vec![]),
            (Playing, "Refused", refusal(), Yes, Flight, convert()),
            (Playing, "FlightLanded", landed(), No, Playing, vec![]),
            (Playing, "FlightRefused", RecoveryEvent::FlightRefused, No, Playing, vec![]),
            (Playing, "FirstPicture", RecoveryEvent::FirstPicture, No, Playing, vec![]),
            (Playing, "LoadStarted", RecoveryEvent::LoadStarted, No, Playing, vec![]),
            (Failed, "Refused", refusal(), No, Failed, vec![]),
            (Failed, "FlightLanded", landed(), No, Failed, vec![]),
            (Failed, "FlightRefused", RecoveryEvent::FlightRefused, No, Failed, vec![]),
            (Failed, "FirstPicture", RecoveryEvent::FirstPicture, No, Failed, vec![]),
            (Failed, "LoadStarted", RecoveryEvent::LoadStarted, Yes, Opening, vec![]),
            // A refusal that NAMES the codec, while a proof is pending: the Named write, never the Proof.
            (Proving, "Refused", named_refusal(), Yes, Failed, vec![named(dts6()), RecoveryEffect::ShowError { refusal: NATIVE }]),
        ];
        let pairs: std::collections::HashSet<String> = table.iter().map(|row| format!("{:?} x {}", row.0, row.1)).collect();
        assert_eq!(pairs.len(), 5 * 5, "every pair of five states and five events is pinned (an extra row pins a variant)");
        for (state, name, ev, handled, next, effects) in table {
            let mut m = in_phase(state);
            let (got, fx) = run(&mut m, ev);
            assert_eq!((got, m.phase(), fx), (handled, next, effects), "{state:?} x {name}");
        }
    }

    /// **A clean audio facet flight proves, once, at the first picture.** The 601 converts DTS 5.1,
    /// PMS answers the audio lane `transcode` and the video lane `copy`, and the reload's first
    /// picture reports exactly one proof; a second picture says nothing.
    #[test]
    fn a_clean_audio_facet_flight_reports_one_proof_at_the_first_picture() {
        let mut m = machine();
        let (_, fx) = run(&mut m, refused(SHUT, on_dts(facts(false, false, true))));
        assert_eq!(fx, [RecoveryEffect::ConvertFacet { facet: dts6(), flight: 1 }]);
        assert_eq!(run(&mut m, landed_with(CLEAN)), (Handled::Yes, vec![]));
        assert_eq!(m.phase(), Phase::Proving);
        assert_eq!(run_proof(&mut m, RecoveryEvent::FirstPicture), (Handled::Yes, vec![proof(dts6())]));
        assert_eq!(m.phase(), Phase::Playing);
        assert_eq!(run(&mut m, RecoveryEvent::FirstPicture), (Handled::No, vec![]), "once");
        // `directplay` on the video lane is as clean as `copy`.
        let mut m = machine();
        run(&mut m, refused(SHUT, on_dts(facts(false, false, true))));
        run(&mut m, landed_with(Lanes { video: Lane::DirectPlay, audio: Lane::Transcode }));
        assert_eq!(m.phase(), Phase::Proving);
    }

    /// **Only the facet rung proves.** The both-facets rungs (HLS or the manual encode), the contingency after a refused
    /// facet flight and the rollback land to `Opening` however clean their verdicts, so their first
    /// picture reports nothing.
    #[test]
    fn the_both_facets_and_rollback_rungs_report_no_proof() {
        let picture_after = |m: &mut OpenRecovery| {
            let fx = run_proof(m, RecoveryEvent::FirstPicture).1;
            (fx, m.phase())
        };
        // (a) an AAC cold Auto Original: nothing suspect, the HLS contingency flies.
        let mut m = machine();
        let (_, fx) = run(&mut m, refused(SHUT, OpenFacts { audio: audio("aac", 2), ..facts(false, true, true) }));
        assert_eq!(fx, [RecoveryEffect::StartAutoHls]);
        run(&mut m, landed_with(CLEAN));
        assert_eq!(picture_after(&mut m), (vec![], Phase::Playing), "(a) the both-facets rung");
        // (b) the facet flight refused: the HLS contingency flies and lands.
        let mut m = machine();
        run(&mut m, refused(SHUT, on_dts(facts(false, true, true))));
        let (_, fx) = run(&mut m, RecoveryEvent::FlightRefused);
        assert_eq!(fx, [RecoveryEffect::StartAutoHls]);
        run(&mut m, landed_with(CLEAN));
        assert_eq!(picture_after(&mut m), (vec![], Phase::Playing), "(b) the contingency after a refused facet flight");
        // (c) a pending Original trial: the rollback.
        let mut m = machine();
        let (_, fx) = run(&mut m, refused(SHUT, on_dts(facts(true, true, true))));
        assert_eq!(fx, [RecoveryEffect::Rollback]);
        run(&mut m, landed_with(CLEAN));
        assert_eq!(picture_after(&mut m), (vec![], Phase::Playing), "(c) the rollback");
    }

    /// A timeout or a source failure names no facet, so neither enters the facet rung nor proves.
    #[test]
    fn a_timeout_or_source_failure_reports_no_proof() {
        for refusal in [LoadRefusal::TimedOut, LoadRefusal::SourceFailed] {
            let mut m = machine();
            let (_, fx) = run(&mut m, refused(refusal, on_dts(facts(false, true, true))));
            assert_eq!(fx, [RecoveryEffect::StartAutoHls], "{refusal}");
            run(&mut m, landed_with(CLEAN));
            let (_, fx) = run_proof(&mut m, RecoveryEvent::FirstPicture);
            assert_eq!((fx, m.phase()), (vec![], Phase::Playing), "{refusal}");
        }
    }

    /// A landing that did not change the converted facet ALONE, by PMS's own verdicts, proves
    /// nothing: the video was transcoded too, a lane is unstated, the audio was not converted, or
    /// the landing carries no verdicts.
    #[test]
    fn a_facet_flight_not_changed_alone_reports_no_proof() {
        let lanes = |video, audio| Some(Lanes { video, audio });
        for (what, landing) in [
            ("both lanes transcoded", lanes(Lane::Transcode, Lane::Transcode)),
            ("video unstated", lanes(Lane::Unstated, Lane::Transcode)),
            ("audio copied: the flip was not the facet's", lanes(Lane::Copy, Lane::Copy)),
            ("audio unstated", lanes(Lane::Copy, Lane::Unstated)),
            ("no verdicts", None),
        ] {
            let mut m = machine();
            run(&mut m, refused(SHUT, on_dts(facts(false, false, true))));
            run(&mut m, RecoveryEvent::FlightLanded { lanes: landing });
            assert_eq!(m.phase(), Phase::Opening, "{what}");
            assert_eq!(run_proof(&mut m, RecoveryEvent::FirstPicture).1, vec![], "{what}");
        }
    }

    /// The proving reload is refused again: the proof is dropped, and the ladder rules as for any
    /// `Opening` (the facet rung is spent: the read-out on a manual Original, the HLS contingency
    /// on a cold Auto Original, whose later picture proves nothing).
    #[test]
    fn a_refused_proving_reload_reports_no_proof() {
        let mut m = in_phase(Phase::Proving);
        let (handled, fx) = run(&mut m, refused(SHUT, on_dts(facts(false, false, true))));
        assert_eq!((handled, fx, m.phase()), (Handled::Yes, ERROR.to_vec(), Phase::Failed));
        let mut m = in_phase(Phase::Proving);
        let (_, fx) = run(&mut m, refused(SHUT, on_dts(facts(false, true, true))));
        assert_eq!((fx, m.phase()), (vec![RecoveryEffect::StartAutoHls], Phase::Flight));
        run(&mut m, landed_with(CLEAN));
        assert_eq!(run_proof(&mut m, RecoveryEvent::FirstPicture).1, vec![], "the dropped proof is not credited later");
    }

    /// **A refusal that names the codec is recorded and picks its rung in ONE step**, learner first:
    /// there is no confirmation state to wait in.
    #[test]
    fn a_named_refusal_confirms_and_picks_its_rung_in_one_step() {
        let mut m = OpenRecovery::new(opus_start());
        let (handled, fx) = run(&mut m, refused(NATIVE, names_opus(facts(false, false, true))));
        assert_eq!(handled, Handled::Yes);
        assert_eq!(fx, [named(opus6()), RecoveryEffect::ConvertFacet { facet: opus6(), flight: 1 }]);
        assert_eq!(m.phase(), Phase::Flight);
        assert_eq!(*m.attempt(), opus_start().without(&opus6()));
    }

    #[test]
    fn a_refused_facet_flight_takes_the_next_rung_but_a_refused_rollback_ends() {
        let facet = |auto: bool| {
            let mut m = machine();
            run(&mut m, refused(SHUT, on_dts(facts(false, auto, true))));
            assert_eq!(m.phase(), Phase::Flight);
            run(&mut m, RecoveryEvent::FlightRefused)
        };
        assert_eq!(facet(true).1, [RecoveryEffect::StartAutoHls], "a refused conversion dispatch still has the HLS contingency");
        assert_eq!(facet(false).1, ERROR);
        let mut m = machine();
        run(&mut m, refused(SHUT, on_dts(facts(true, true, true))));
        let (_, fx) = run(&mut m, RecoveryEvent::FlightRefused);
        assert_eq!((fx, m.phase()), (ERROR.to_vec(), Phase::Failed), "a refused rollback stays terminal");
    }

    #[test]
    fn a_timeout_names_no_facet() {
        let timed_out = |auto| run(&mut machine(), refused(LoadRefusal::TimedOut, on_dts(facts(false, auto, true)))).1;
        assert_eq!(timed_out(true), [RecoveryEffect::StartAutoHls]);
        assert_eq!(timed_out(false), [RecoveryEffect::ShowError { refusal: LoadRefusal::TimedOut }]);
    }

    #[test]
    fn forced_ends_on_the_read_out_after_rollback_only() {
        // A set with no DTS row: the audio is the one the refusal can name.
        let forced = |pending, refusal| {
            run(&mut OpenRecovery::new(Arc::new(Caps::assumed())), refused(refusal, OpenFacts { forced: true, ..on_dts(facts(pending, false, true)) })).1
        };
        for refusal in [SHUT, LoadRefusal::TimedOut, LoadRefusal::Stalled, LoadRefusal::SourceFailed] {
            assert_eq!(forced(true, refusal), [RecoveryEffect::Rollback], "{refusal}");
            let named = refusal.names_pipeline().then(|| RecoveryEffect::NameRefusedAudio { codec: "dca".into() });
            let want: Vec<_> = named.into_iter().chain([RecoveryEffect::ShowError { refusal }]).collect();
            assert_eq!(forced(false, refusal), want, "{refusal}");
        }
    }

    /// **Force's read-out names the audio only when the refusal is attributable to it.** LG's `num=601`
    /// is also a video/resource refusal, so the codec is named on positive evidence (the set does not
    /// play it from the file, it is not the app's PCM decode, the video is not the likelier cause) and
    /// never by elimination; every other refusal keeps the generic read-out. Scheme #695 rows 2, 3, 6, 10.
    #[test]
    fn forced_names_the_audio_only_when_the_refusal_is_attributable_to_it() {
        let h264 = |w, h| Some(StreamVideo { codec: "h264".into(), width: w, height: h });
        let base = |audio, video| OpenFacts { forced: true, direct: true, audio, video, ..facts(false, false, true) };
        let assumed = || OpenRecovery::new(Arc::new(Caps::assumed()));
        let end = |mut m: OpenRecovery, f: OpenFacts| run(&mut m, refused(SHUT, f)).1;
        let named = |codec: &str| {
            vec![RecoveryEffect::NameRefusedAudio { codec: codec.into() }, RecoveryEffect::ShowError { refusal: SHUT }]
        };
        // Positive: a set with no DTS row, TrueHD, an FHD video that is not the suspect.
        assert_eq!(end(assumed(), base(audio("dca", 6), h264(1920, 1080))), named("dca"));
        assert_eq!(end(assumed(), base(audio("truehd", 8), None)), named("truehd"));
        // Row 2: Opus plays under Force on a table that lists it.
        assert_eq!(end(OpenRecovery::new(opus_start()), base(audio("opus", 2), h264(1920, 1080))), ERROR, "opus");
        // Row 6: FLAC/ALAC are decoded by the app and the TV is fed PCM.
        assert_eq!(end(assumed(), base(audio("flac", 2), h264(1920, 1080))), ERROR, "flac");
        assert_eq!(end(assumed(), base(audio("alac", 2), None)), ERROR, "alac");
        // Row 3: a set with a DTS row plays the DTS core.
        assert_eq!(end(machine(), base(audio("dca", 6), h264(1920, 1080))), ERROR, "dts on a DTS-decoder set");
        // A 4K H.264 refused with 601 is the video's known refusal, whatever the audio.
        assert_eq!(end(assumed(), base(audio("dca", 6), h264(3840, 2160))), ERROR, "video 601");
        assert_eq!(end(assumed(), base(audio("truehd", 8), h264(3840, 2160))), ERROR, "video 601, truehd");
        // A Stalled Load is audio's alone: the video is no suspect for it.
        let stalled = run(&mut assumed(), refused(LoadRefusal::Stalled, base(audio("dca", 6), h264(3840, 2160)))).1;
        assert_eq!(stalled[0], RecoveryEffect::NameRefusedAudio { codec: "dca".into() });
    }

    #[test]
    fn rollback_comes_first_and_is_not_counted() {
        let mut m = machine();
        let (_, fx) = run(&mut m, refused(SHUT, on_dts(facts(true, false, true))));
        assert_eq!(fx, [RecoveryEffect::Rollback]);
        run(&mut m, landed());
        let (_, fx) = run(&mut m, refused(SHUT, on_dts(facts(false, false, true))));
        assert_eq!(fx, [RecoveryEffect::ConvertFacet { facet: dts6(), flight: 1 }], "the rollback spent no flight");
    }

    #[test]
    fn a_source_failure_keeps_mains_two_arms() {
        let failed = |m: &mut OpenRecovery, auto| {
            run(m, refused(LoadRefusal::SourceFailed, on_dts(facts(false, auto, true)))).1
        };
        assert_eq!(failed(&mut machine(), true), [RecoveryEffect::StartAutoHls]);
        assert_eq!(failed(&mut machine(), false), [RecoveryEffect::ShowError { refusal: LoadRefusal::SourceFailed }]);
        let mut m = machine();
        run(&mut m, refused(SHUT, on_dts(facts(false, true, true))));
        run(&mut m, landed());
        assert_eq!(failed(&mut m, true), [RecoveryEffect::StartAutoHls]);
    }

    #[test]
    fn a_facet_the_attempt_already_lacks_is_no_rung() {
        let start = Arc::new(opus_start().without(&opus6()));
        assert_eq!(start.without(&opus6()), *start, "the fixture: the attempt already lacks the facet");
        let stalled = |auto| {
            let mut m = OpenRecovery::new(start.clone());
            run(&mut m, refused(LoadRefusal::Stalled, names_opus(facts(false, auto, true)))).1
        };
        let read_out = RecoveryEffect::ShowError { refusal: LoadRefusal::Stalled };
        assert_eq!(stalled(false), [named(opus6()), read_out], "the write is asked for, the rung is the read-out");
        assert_eq!(stalled(true), [named(opus6()), RecoveryEffect::StartAutoHls]);
    }

    #[test]
    fn a_failed_original_open_prefers_rollback_then_cold_auto_before_error() {
        assert_eq!(rung(true, false, true, true), [RecoveryEffect::Rollback]);
        assert_eq!(rung(false, false, true, true), [RecoveryEffect::StartAutoHls]);
        assert_eq!(rung(false, false, true, false), ERROR, "a post-frame failure is not a cold open");
        assert_eq!(rung(false, false, false, true), ERROR, "manual Original stays the user's fixed choice");
        // A suspect audio track converts first, on a cold Auto Original too (the full HLS fallback
        // would also convert it, but throws the route away), and on a manual Original.
        assert_eq!(rung(false, true, true, true), [RecoveryEffect::ConvertFacet { facet: dts6(), flight: 1 }]);
        assert_eq!(rung(false, true, false, true), [RecoveryEffect::ConvertFacet { facet: dts6(), flight: 1 }]);
        // The rollback of a pending HLS to Original still comes first: its HLS route is the one a
        // failed open owes the viewer, and its audio is the server's.
        assert_eq!(rung(true, true, true, true), [RecoveryEffect::Rollback]);
    }

    #[test]
    fn a_refusal_that_names_native_audio_is_confirmed_before_the_rung_is_picked() {
        // Nothing suspect was fed (the facts name no stream audio): the ladder for a manual
        // Original with nothing pending is the read-out, after the write.
        let mut m = machine();
        let (handled, fx) = run(&mut m, refused(NATIVE, OpenFacts { native_audio: Some(opus6()), ..facts(false, false, true) }));
        assert_eq!((handled, m.phase()), (Handled::Yes, Phase::Failed));
        assert_eq!(fx, [named(opus6()), RecoveryEffect::ShowError { refusal: NATIVE }]);
    }

    #[test]
    fn a_known_combination_still_takes_the_auto_originals_hls_contingency() {
        let mut m = machine();
        let (_, fx) = run(&mut m, refused(NATIVE, OpenFacts { native_audio: Some(opus6()), ..facts(false, true, true) }));
        assert_eq!((fx, m.phase()), (vec![named(opus6()), RecoveryEffect::StartAutoHls], Phase::Flight));
    }

    #[test]
    fn a_flight_ends_in_a_reload_or_in_the_read_out() {
        let mut m = machine();
        run(&mut m, refused(SHUT, facts(true, false, true)));
        assert_eq!(m.phase(), Phase::Flight);
        let (handled, fx) = run(&mut m, landed());
        assert_eq!((handled, fx, m.phase()), (Handled::Yes, vec![], Phase::Opening));

        let mut m = machine();
        run(&mut m, refused(SHUT, facts(false, true, true)));
        let (handled, fx) = run(&mut m, RecoveryEvent::FlightRefused);
        assert_eq!((handled, m.phase()), (Handled::Yes, Phase::Failed));
        assert_eq!(fx, ERROR, "the read-out names the refusal that started the flight");
    }

    #[test]
    fn a_failure_while_a_flight_or_the_read_out_is_outstanding_is_not_handled() {
        let mut m = machine();
        run(&mut m, refused(SHUT, facts(true, false, true)));
        assert_eq!(run(&mut m, refused(SHUT, facts(false, false, true))), (Handled::No, vec![]));
        assert_eq!(m.phase(), Phase::Flight);
        run(&mut m, RecoveryEvent::FlightRefused);
        assert_eq!(run(&mut m, refused(SHUT, facts(true, false, true))), (Handled::No, vec![]));
        assert_eq!(m.phase(), Phase::Failed);
    }

    #[test]
    fn answers_with_no_question_outstanding_are_not_handled() {
        let mut m = machine();
        for ev in [landed(), RecoveryEvent::FlightRefused] {
            assert_eq!(run(&mut m, ev), (Handled::No, vec![]));
        }
        assert_eq!(m.phase(), Phase::Opening);
    }

    #[test]
    fn the_first_picture_ends_the_ladder_once() {
        let mut m = machine();
        assert_eq!(run(&mut m, RecoveryEvent::FirstPicture), (Handled::Yes, vec![]));
        assert_eq!(m.phase(), Phase::Playing);
        assert_eq!(run(&mut m, RecoveryEvent::FirstPicture), (Handled::No, vec![]), "idempotent");
        // A failure of an Engine that HAS a picture (`no_frames` false) runs the same ladder, whose
        // facts stop it at the read-out: neither the facet rung nor the HLS contingency applies to a
        // stream that was playing (the suspect is not the reason a playing stream failed).
        let (handled, fx) = run(&mut m, refused(LoadRefusal::SourceFailed, facts(false, true, false)));
        assert_eq!((handled, m.phase()), (Handled::Yes, Phase::Failed));
        assert_eq!(fx, [RecoveryEffect::ShowError { refusal: LoadRefusal::SourceFailed }]);
        let mut m = machine();
        run(&mut m, RecoveryEvent::FirstPicture);
        assert_eq!(run(&mut m, refused(SHUT, on_dts(facts(false, false, false)))).1, ERROR);
    }

    /// The machine lives per REQUEST but a picture is per ENGINE (`frames` and `seen_frame` are
    /// cleared by every reload): a request that showed a picture and then reloads (a track pick, a
    /// seek that escalates) owes the new Engine the same open ladder, so a refusal of THAT Engine,
    /// before ITS picture (`no_frames`), converts the suspect facet. The facet rung is gated on the
    /// refusal's own Engine having no picture, not on the request's history.
    #[test]
    fn a_reload_with_no_picture_is_pre_picture_for_its_own_engine() {
        let start = opus_start();
        assert_ne!(start.without(&opus6()), *start);
        let mut m = OpenRecovery::new(start.clone());
        run(&mut m, RecoveryEvent::FirstPicture);
        assert_eq!(m.phase(), Phase::Playing);
        let reloaded = names_opus(facts(false, false, true));
        let (handled, fx) = run(&mut m, refused(NATIVE, reloaded.clone()));
        assert_eq!(
            (handled, fx, m.phase()),
            (Handled::Yes, vec![named(opus6()), RecoveryEffect::ConvertFacet { facet: opus6(), flight: 1 }], Phase::Flight)
        );
        assert_eq!(*m.attempt(), start.without(&opus6()));
        // The same refusal of an Engine that HAS a picture converts nothing.
        let mut m = OpenRecovery::new(start);
        run(&mut m, RecoveryEvent::FirstPicture);
        let (_, fx) = run(&mut m, refused(NATIVE, names_opus(facts(false, false, false))));
        assert_eq!((fx, m.phase()), (vec![named(opus6()), RecoveryEffect::ShowError { refusal: NATIVE }], Phase::Failed));
    }

    #[test]
    fn a_stall_is_a_refusal_like_any_other_even_when_the_install_already_knew_the_combination() {
        // A known combination converts nothing (it is in `start` already, test above), so a manual
        // Original ends in the read-out and the viewer never waits on a spinner for ever. The rungs
        // above the read-out still apply: a pending Original trial's rollback, a cold Auto
        // Original's HLS contingency.
        let stalled = |pending, auto| {
            let mut m = OpenRecovery::new(Arc::new(opus_start().without(&opus6())));
            run(&mut m, refused(LoadRefusal::Stalled, names_opus(facts(pending, auto, true)))).1
        };
        assert_eq!(stalled(false, false), [named(opus6()), RecoveryEffect::ShowError { refusal: LoadRefusal::Stalled }]);
        assert_eq!(stalled(true, false), [named(opus6()), RecoveryEffect::Rollback]);
        assert_eq!(stalled(false, true), [named(opus6()), RecoveryEffect::StartAutoHls]);
    }

    /// A machine in each state, built through its own events: `Opening`, `Proving` (the facet flight
    /// landed clean), `Flight` (the facet rung; a DTS 5.1 refused), `Playing` and `Failed` (the facet
    /// flight refused on a manual Original).
    fn in_phase(phase: Phase) -> OpenRecovery {
        let mut m = machine();
        match phase {
            Phase::Opening => {}
            Phase::Proving => {
                run(&mut m, refused(SHUT, on_dts(facts(false, false, true))));
                run(&mut m, landed_with(CLEAN));
            }
            Phase::Flight => {
                run(&mut m, refused(SHUT, on_dts(facts(false, false, true))));
            }
            Phase::Playing => {
                run(&mut m, RecoveryEvent::FirstPicture);
            }
            Phase::Failed => {
                run(&mut m, refused(SHUT, on_dts(facts(false, false, true))));
                run(&mut m, RecoveryEvent::FlightRefused);
            }
        }
        assert_eq!(m.phase(), phase, "the fixture");
        m
    }

    /// **A Load the machine did not ask for re-opens the ladder.** Home during a facet flight
    /// clears the flight (`begin_engine_teardown`), and the foreground restore Loads the same
    /// refused route: the machine was left in `Flight` (its `Refused` is `Handled::No`, the Engine
    /// spun on Connecting for good). The same restore after the read-out was up left `Failed`.
    /// `LoadStarted` moves both back to `Opening`, keeping what the request spent: its flights and
    /// the facet it took out.
    #[test]
    fn a_load_the_machine_did_not_ask_for_reopens_the_ladder_keeping_the_spent_rungs() {
        let start = dts_start();
        for phase in [Phase::Flight, Phase::Failed] {
            let mut m = in_phase(phase);
            assert_eq!(*m.attempt(), start.without(&dts6()), "{phase:?}: the facet rung narrowed the attempt");
            assert_eq!(run(&mut m, RecoveryEvent::LoadStarted), (Handled::Yes, vec![]), "{phase:?}");
            assert_eq!(m.phase(), Phase::Opening, "{phase:?}");
            assert_eq!(*m.attempt(), start.without(&dts6()), "{phase:?}: the excluded facet is kept");
            // The restored Load is refused again: the facet rung is spent (one flight), so a manual
            // Original ends on the read-out and a cold Auto Original takes its HLS contingency.
            let refused_again = |auto| refused(SHUT, on_dts(facts(false, auto, true)));
            let (handled, fx) = run(&mut m.clone(), refused_again(false));
            assert_eq!((handled, fx), (Handled::Yes, ERROR.to_vec()), "{phase:?}: no second conversion");
            let (handled, fx) = run(&mut m, refused_again(true));
            assert_eq!((handled, fx, m.phase()), (Handled::Yes, vec![RecoveryEffect::StartAutoHls], Phase::Flight), "{phase:?}");
        }
    }

    /// `LoadStarted` is a pinned outcome in every state: `Flight` and `Failed` re-open, `Proving`
    /// takes the landing's own reload (the phase stays), and `Opening` and `Playing` ignore it.
    #[test]
    fn load_started_is_pinned_in_every_state() {
        for (phase, handled, next) in [
            (Phase::Opening, Handled::No, Phase::Opening),
            (Phase::Proving, Handled::Yes, Phase::Proving),
            (Phase::Flight, Handled::Yes, Phase::Opening),
            (Phase::Playing, Handled::No, Phase::Playing),
            (Phase::Failed, Handled::Yes, Phase::Opening),
        ] {
            let mut m = in_phase(phase);
            assert_eq!(run(&mut m, RecoveryEvent::LoadStarted), (handled, vec![]), "{phase:?}");
            assert_eq!(m.phase(), next, "{phase:?}");
        }
    }

    /// The landing's own reload passes the same funnel: `FlightLanded` has moved the machine to
    /// `Opening` first, so the `LoadStarted` it raises changes nothing and the flight stays spent.
    #[test]
    fn the_flights_own_reload_changes_nothing() {
        let mut m = in_phase(Phase::Flight);
        run(&mut m, landed());
        assert_eq!(run(&mut m, RecoveryEvent::LoadStarted), (Handled::No, vec![]));
        assert_eq!(m.phase(), Phase::Opening);
        let (_, fx) = run(&mut m, refused(SHUT, on_dts(facts(false, false, true))));
        assert_eq!(fx, ERROR, "one conversion per request, whichever way the second open began");
    }

    /// **A request that already showed a picture proves nothing by a later facet flight.** The
    /// picture is per Engine (`frames` and `seen_frame` are cleared by every reload), so a facet
    /// rung is reachable from `Playing`: a DTS 5.1 direct play that pictured, a Home and a return,
    /// and the restore Load refused (`601`) before its picture. This request already played DTS 5.1
    /// natively, so the conversion's rescue proves nothing about the format.
    #[test]
    fn a_request_that_already_pictured_never_proves_a_facet_flight() {
        let mut m = in_phase(Phase::Playing);
        let (handled, fx) = run(&mut m, refused(SHUT, on_dts(facts(false, false, true))));
        assert_eq!((handled, fx), (Handled::Yes, vec![RecoveryEffect::ConvertFacet { facet: dts6(), flight: 1 }]));
        assert_eq!(run(&mut m, landed_with(CLEAN)), (Handled::Yes, vec![]));
        assert_eq!(m.phase(), Phase::Opening, "a clean landing after a picture is not a proof");
        assert_eq!(run_proof(&mut m, RecoveryEvent::FirstPicture).1, vec![], "no proof at the converted route's picture");
        assert_eq!(m.phase(), Phase::Playing);
    }

    /// **Only the landing's own reload keeps a pending proof.** The machine moves to `Proving` at
    /// `FlightLanded`, and the landing then reloads onto the converted route (its `LoadStarted`
    /// arrives AFTER the move: `land_recovery_flight`). A second Load before the picture (a track
    /// switch, a restore after Home, a seek reload) is a route whose lanes were never read.
    #[test]
    fn only_the_landings_own_reload_keeps_the_pending_proof() {
        let mut m = in_phase(Phase::Proving);
        assert_eq!(run(&mut m, RecoveryEvent::LoadStarted), (Handled::Yes, vec![]), "the landing's own reload");
        assert_eq!(m.phase(), Phase::Proving);
        assert_eq!(run_proof(&mut m, RecoveryEvent::FirstPicture), (Handled::Yes, vec![proof(dts6())]));
    }

    #[test]
    fn a_reload_the_landing_did_not_ask_for_drops_the_pending_proof() {
        let mut m = in_phase(Phase::Proving);
        run(&mut m, RecoveryEvent::LoadStarted);
        assert_eq!(run(&mut m, RecoveryEvent::LoadStarted), (Handled::Yes, vec![]), "a foreign Load");
        assert_eq!(m.phase(), Phase::Opening);
        assert_eq!(run_proof(&mut m, RecoveryEvent::FirstPicture).1, vec![], "the dropped proof is not credited");
        // The spent rungs are kept: a refusal of the foreign Load is ruled on as any `Opening` is.
        let mut m = in_phase(Phase::Proving);
        run(&mut m, RecoveryEvent::LoadStarted);
        run(&mut m, RecoveryEvent::LoadStarted);
        let (_, fx) = run(&mut m, refused(SHUT, on_dts(facts(false, false, true))));
        assert_eq!(fx, ERROR, "one conversion per request");
    }

    #[test]
    fn a_request_starts_on_the_value_it_was_given() {
        let start = Arc::new(Caps::assumed());
        let m = OpenRecovery::new(start.clone());
        assert!(Arc::ptr_eq(&m.attempt(), &start), "no narrowing, no copy");
        assert_eq!(m.phase(), Phase::Opening);
    }

    #[test]
    fn a_refusal_prints_its_number() {
        assert_eq!(LoadRefusal::Refused { num: 601 }.to_string(), "type=18 num=601");
    }

    // ---- the report of a rescued refusal (S6, #1075) ----------------------------------------

    fn recovered(rung: Rung, refusal: LoadRefusal, facet: Option<Facet>) -> RecoveryEffect {
        RecoveryEffect::ReportRecovered(Recovered { rung, refusal, facet })
    }

    /// Every `ReportRecovered` in `fx`.
    fn reports(fx: &[RecoveryEffect]) -> Vec<RecoveryEffect> {
        fx.iter().filter(|e| matches!(e, RecoveryEffect::ReportRecovered(_))).cloned().collect()
    }

    /// **A play the facet rung rescued is reported once, at its first picture, with the refusal the
    /// pipeline gave and the stream it named.** Opening (no proof) and Proving (a clean audio facet
    /// landing) rescue alike; the proof's own write still comes first.
    #[test]
    fn a_facet_rescue_reports_at_the_first_picture() {
        let mut m = machine();
        run(&mut m, refused(SHUT, on_dts(facts(false, false, true))));
        run(&mut m, landed());
        assert_eq!(m.phase(), Phase::Opening);
        let (_, fx) = run(&mut m, RecoveryEvent::FirstPicture);
        assert_eq!(fx, [recovered(Rung::Facet, SHUT, Some(dts6()))]);

        let mut m = machine();
        run(&mut m, refused(SHUT, on_dts(facts(false, false, true))));
        run(&mut m, landed_with(CLEAN));
        run(&mut m, RecoveryEvent::LoadStarted);
        let (_, fx) = run(&mut m, RecoveryEvent::FirstPicture);
        assert_eq!(fx, [proof(dts6()), recovered(Rung::Facet, SHUT, Some(dts6()))], "the proof's write, then the report");
    }

    /// Nothing is reported before a picture shows: a landing is not a rescue.
    #[test]
    fn nothing_is_reported_before_the_rescue_shows_a_picture() {
        let mut m = machine();
        let (_, fx) = run(&mut m, refused(SHUT, on_dts(facts(false, false, true))));
        assert_eq!(reports(&fx), []);
        assert_eq!(reports(&run(&mut m, landed()).1), []);
        assert_eq!(reports(&run(&mut m, RecoveryEvent::LoadStarted).1), []);
    }

    /// **A play with no refusal reports nothing.**
    #[test]
    fn a_play_that_was_never_refused_reports_nothing() {
        let mut m = machine();
        assert_eq!(run(&mut m, RecoveryEvent::FirstPicture), (Handled::Yes, vec![]));
        assert_eq!(run(&mut m, RecoveryEvent::FirstPicture), (Handled::No, vec![]));
    }

    /// **One report per request**, however many Engines or rescues the request goes through: a
    /// reload after the first picture that is refused and rescued again is the same play.
    #[test]
    fn a_request_reports_one_rescue_even_when_a_later_reload_is_rescued_too() {
        let mut m = machine();
        run(&mut m, refused(SHUT, on_dts(facts(false, false, true))));
        run(&mut m, landed());
        assert_eq!(reports(&run(&mut m, RecoveryEvent::FirstPicture).1).len(), 1);
        // A track pick reloads; the reload's Engine is refused and a cold Auto Original's HLS
        // contingency rescues it.
        let (_, fx) = run(&mut m, refused(SHUT, on_dts(facts(false, true, true))));
        assert_eq!(fx, [RecoveryEffect::StartAutoHls]);
        run(&mut m, landed());
        let (_, fx) = run(&mut m, RecoveryEvent::FirstPicture);
        assert_eq!(fx, [], "the request already reported its rescue");
    }

    /// A refusal that ends on the read-out is no rescue, and a Load the machine did not ask for
    /// (the foreground restore of the refused route) that then plays is not credited to a rung.
    #[test]
    fn a_ladder_that_read_out_reports_nothing_even_if_a_restored_load_plays() {
        let mut m = machine();
        run(&mut m, refused(SHUT, on_dts(facts(false, false, true))));
        run(&mut m, landed());
        let (_, fx) = run(&mut m, refused(SHUT, on_dts(facts(false, false, true))));
        assert_eq!(fx, ERROR);
        assert_eq!(m.phase(), Phase::Failed);
        run(&mut m, RecoveryEvent::LoadStarted);
        assert_eq!(reports(&run(&mut m, RecoveryEvent::FirstPicture).1), []);

        // The same for a flight Home abandoned.
        let mut m = machine();
        run(&mut m, refused(SHUT, on_dts(facts(false, false, true))));
        assert_eq!(m.phase(), Phase::Flight);
        run(&mut m, RecoveryEvent::LoadStarted);
        assert_eq!(reports(&run(&mut m, RecoveryEvent::FirstPicture).1), []);
    }

    /// **The manual both-facets encode is reported as its own rung (`Encode`), not as the HLS
    /// contingency (`Both`).** A refused 4K H.264 whose facet flight the server refused, then the
    /// encode: "this stream was refused, the encode played"; and a refusal with nothing to lower
    /// (the 1080p frame-rate case) reports its stream and the encode too.
    #[test]
    fn a_manual_both_facets_rescue_reports_the_encode_rung() {
        let mut m = manual_machine();
        let cause = LoadRefusal::Refused { num: 100 };
        run(&mut m, refused(cause, on_4k("ac3", false)));
        run(&mut m, landed());
        let (_, fx) = run(&mut m, RecoveryEvent::FirstPicture);
        assert_eq!(fx, [recovered(Rung::Facet, cause, Some(h264_4k_facet()))], "the video facet rescued it");

        let mut m = machine();
        run(&mut m, refused(cause, on_4k("dca", false)));
        run(&mut m, landed());
        run(&mut m, refused(cause, on_4k("ac3", false)));
        assert_eq!(m.phase(), Phase::Flight);
        run(&mut m, landed());
        let (_, fx) = run(&mut m, RecoveryEvent::FirstPicture);
        assert_eq!(fx, [recovered(Rung::Encode, cause, Some(dts6()))], "the FIRST refusal's stream, the LAST rung");

        let mut m = manual_machine();
        run(&mut m, refused(SHUT, manual_on("ac3", h264_1080p())));
        run(&mut m, landed());
        let (_, fx) = run(&mut m, RecoveryEvent::FirstPicture);
        assert_eq!(fx, [recovered(Rung::Encode, SHUT, Some(Facet::Video { codec: "h264".into(), width: 1920, height: 1080 }))]);

        // The encode's own refusal reads out and reports nothing.
        let mut m = manual_machine();
        run(&mut m, refused(SHUT, manual_on("ac3", h264_1080p())));
        run(&mut m, landed());
        run(&mut m, refused(SHUT, manual_on("ac3", None)));
        assert_eq!(reports(&run(&mut m, RecoveryEvent::FirstPicture).1), []);
    }

    /// The rung that played is the LAST one, the refusal is the FIRST: a facet flight the server
    /// refused and the HLS contingency after it report "this stream was refused, HLS played".
    #[test]
    fn a_later_rung_replaces_the_rung_and_never_the_refusal() {
        let mut m = machine();
        run(&mut m, refused(SHUT, on_dts(facts(false, true, true))));
        // A cold Auto Original's refusal of DTS took the facet rung first; its flight is refused.
        let (_, fx) = run(&mut m, RecoveryEvent::FlightRefused);
        assert_eq!(fx, [RecoveryEffect::StartAutoHls]);
        run(&mut m, landed());
        let (_, fx) = run(&mut m, RecoveryEvent::FirstPicture);
        assert_eq!(fx, [recovered(Rung::Both, SHUT, Some(dts6()))]);
    }

    /// A video facet: only a numbered refusal names the video, and the stream class is the codec's.
    #[test]
    fn a_video_facet_rescue_names_the_source_video() {
        let mut m = manual_machine();
        run(&mut m, refused(SHUT, on_4k("aac", false)));
        run(&mut m, landed());
        let (_, fx) = run(&mut m, RecoveryEvent::FirstPicture);
        assert_eq!(fx, [recovered(Rung::Facet, SHUT, Some(h264_4k_facet()))]);
    }

    /// A Load that timed out names NO stream even when the audio fed is one a refusal would suspect:
    /// a slow server is not a DTS problem, and the ladder itself ignores the audio for a timeout.
    #[test]
    fn a_timeout_names_no_stream_even_behind_suspect_audio() {
        let mut m = machine();
        let (_, fx) = run(&mut m, refused(LoadRefusal::TimedOut, OpenFacts { auto_original: true, ..on_dts(facts(false, true, true)) }));
        assert_eq!(fx, [RecoveryEffect::StartAutoHls], "a timeout converts nothing");
        run(&mut m, landed());
        let (_, fx) = run(&mut m, RecoveryEvent::FirstPicture);
        assert_eq!(fx, [recovered(Rung::Both, LoadRefusal::TimedOut, None)]);
    }

    /// A Load the landing did not ask for, before the picture, keeps the rescue: the route is still
    /// the flight's (a track pick or a restore reloads the SAME installed route), and only the
    /// proof, which needs the landing's own reload, is dropped.
    #[test]
    fn a_foreign_load_after_the_landing_keeps_the_rescue_but_drops_the_proof() {
        let mut m = machine();
        run(&mut m, refused(SHUT, on_dts(facts(false, false, true))));
        run(&mut m, landed_with(CLEAN));
        run(&mut m, RecoveryEvent::LoadStarted);
        run(&mut m, RecoveryEvent::LoadStarted);
        assert_eq!(m.phase(), Phase::Opening);
        let (_, fx) = run(&mut m, RecoveryEvent::FirstPicture);
        assert_eq!(fx, [recovered(Rung::Facet, SHUT, Some(dts6()))], "reported, with no proof");
    }

    /// A Load that timed out names no stream and is reported as the timeout it was.
    #[test]
    fn a_timeout_rescued_by_the_hls_contingency_reports_the_timeout_and_no_stream() {
        let mut m = machine();
        let (_, fx) = run(&mut m, refused(LoadRefusal::TimedOut, on_4k("aac", true)));
        assert_eq!(fx, [RecoveryEffect::StartAutoHls]);
        run(&mut m, landed());
        let (_, fx) = run(&mut m, RecoveryEvent::FirstPicture);
        assert_eq!(fx, [recovered(Rung::Both, LoadRefusal::TimedOut, None)]);
    }

    /// The native audio a refusal named is the stream, with its channel count (the stall too).
    #[test]
    fn a_named_native_audio_refusal_reports_that_audio() {
        let mut m = OpenRecovery::new(opus_start());
        let (_, fx) = run(&mut m, refused(LoadRefusal::Stalled, names_opus(facts(false, false, true))));
        assert_eq!(fx, [named(opus6()), RecoveryEffect::ConvertFacet { facet: opus6(), flight: 1 }]);
        run(&mut m, landed());
        let (_, fx) = run(&mut m, RecoveryEvent::FirstPicture);
        assert_eq!(fx, [recovered(Rung::Facet, LoadRefusal::Stalled, Some(opus6()))]);
    }

    /// The Original trial's HLS rollback is a rescue when the TELEVISION refused the trial, and
    /// nothing when the trial failed for the source's own reasons.
    #[test]
    fn a_rollback_is_reported_only_when_the_pipeline_refused_the_trial() {
        let mut m = machine();
        let (_, fx) = run(&mut m, refused(SHUT, facts(true, true, true)));
        assert_eq!(fx, [RecoveryEffect::Rollback]);
        run(&mut m, landed());
        let (_, fx) = run(&mut m, RecoveryEvent::FirstPicture);
        assert_eq!(fx, [recovered(Rung::Rollback, SHUT, None)]);

        let mut m = machine();
        let (_, fx) = run(&mut m, refused(LoadRefusal::SourceFailed, facts(true, true, true)));
        assert_eq!(fx, [RecoveryEffect::Rollback]);
        run(&mut m, landed());
        assert_eq!(reports(&run(&mut m, RecoveryEvent::FirstPicture).1), [], "the demuxer failed, not the television");
    }

    /// A source failure that the HLS contingency rescued is not a refusal; but a pipeline refusal
    /// that follows it on the way to the rescue is the request's refusal.
    #[test]
    fn a_source_failure_is_not_a_refusal_unless_the_pipeline_refuses_later() {
        let mut m = machine();
        run(&mut m, refused(LoadRefusal::SourceFailed, facts(false, true, true)));
        run(&mut m, landed());
        assert_eq!(reports(&run(&mut m, RecoveryEvent::FirstPicture).1), []);

        let mut m = machine();
        run(&mut m, refused(LoadRefusal::SourceFailed, facts(false, true, true)));
        run(&mut m, RecoveryEvent::LoadStarted);
        run(&mut m, landed());
        run(&mut m, refused(SHUT, on_dts(facts(false, true, true))));
        run(&mut m, landed());
        assert_eq!(
            run(&mut m, RecoveryEvent::FirstPicture).1,
            [recovered(Rung::Both, SHUT, Some(dts6()))]
        );
    }
}
