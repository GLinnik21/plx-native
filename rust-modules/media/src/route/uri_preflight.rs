//! Source-specific URI qualification on the existing off-frame resolve worker.
//! A native implementation may answer Unknown until its display and control behavior is proven.
//! Production proves nothing for anyone; the one qualification it supplies is a viewer's own:
//! Settings > Video & playback > Playback engine > Experimental ([`OriginalRequest::uri_opt_in`]).

use std::sync::Arc;
use std::time::{Duration, Instant};

use plx_base::task::OffFrame;
use plx_playback_contract::uri::{Binding, NativeTrackIndex, UriSource};
use plx_playback_contract::{Capabilities, Fidelity, Requirements, SourceKind, TrackChoice, TrackKind, TrackSelection};

use super::SourceSelection;

/// The exact selected subtitle's source format and placement. A native track index alone does
/// not establish that this firmware renders its codec.
#[derive(Clone, PartialEq, Eq)]
pub struct SourceSubtitle { pub codec: String, pub embedded: bool }

/// Captured product evidence shared by boot's executor configuration and one resolve's IO port.
/// Callable operations and certified firmware behavior are separate facts.
#[derive(Clone, Copy)]
pub struct ProductEvidence {
    pub display_exported: bool,
    pub binding: Binding,
    pub callable: Capabilities,
    pub certified: Capabilities,
    pub preview_rectangle: bool,
}

impl ProductEvidence {
    /// Current firmware has a real URI bus implementation, but no qualified production source,
    /// control and release certificate. A display window mode alone cannot certify playback.
    /// Only a viewer's own opt-in ([`opted_in`](Self::opted_in)) certifies anything, and only for
    /// that viewer's requests.
    pub fn unqualified(display_mode: i32) -> Self {
        Self::unqualified_on(display_mode, Binding::Unbound)
    }

    /// [`unqualified`](Self::unqualified) with the display path composition chose for the
    /// executor (the URI driver's `product_binding`). The binding configures the executor and
    /// names the display path an offer would use; it certifies nothing. `certified` stays all
    /// false, so `permits` and `could_qualify_pms` offer no source through any binding.
    pub fn unqualified_on(display_mode: i32, binding: Binding) -> Self {
        let exported = display_mode == plx_platform::tv::sink::VP_EXPORTED;
        let callable = Capabilities { progressive_uri: true, hls_uri: true, packet_feed: false,
            pause: true, seek: true, audio_track: true, subtitle_track: true,
            audio_off: false, subtitle_off: false };
        let certified = Capabilities { progressive_uri: false, hls_uri: false, packet_feed: false,
            pause: false, seek: false, audio_track: false, subtitle_track: false,
            audio_off: false, subtitle_off: false };
        Self { display_exported: exported, binding, callable,
            certified, preview_rectangle: false }
    }

    /// This evidence for a viewer who turned on the URI engine in Settings: the capabilities a URL
    /// carrier needs (the progressive source, pause, seek and audio-track selection), where the
    /// executor can call them, are certified by that choice. Subtitle selection and converted
    /// outputs stay uncertified, and the display path is the composition's own, so an unbound
    /// executor still offers nothing. The URI driver is configured with these capabilities; no
    /// source reaches it without a qualification.
    pub fn opted_in(self) -> Self {
        let carried = |certified: bool, callable: bool| certified || callable;
        Self { certified: Capabilities {
            progressive_uri: carried(self.certified.progressive_uri, self.callable.progressive_uri),
            pause: carried(self.certified.pause, self.callable.pause),
            seek: carried(self.certified.seek, self.callable.seek),
            audio_track: carried(self.certified.audio_track, self.callable.audio_track),
            ..self.certified }, ..self }
    }

    /// Whether the composed display path can show a URI source at all: the exported window with
    /// a real canvas (and the display mode that exports it), or the ACB plane. An unbound
    /// executor shows nothing. This is only the path; what may be offered through it is still
    /// decided by `certified`.
    fn display_path_offerable(&self) -> bool {
        match self.binding {
            Binding::Exported { canvas: (w, h) } => self.display_exported && w > 0 && h > 0,
            Binding::Acb => true,
            Binding::Unbound => false,
        }
    }

    pub fn permits(&self, requirements: Requirements, preview: bool) -> bool {
        self.display_path_offerable()
            && (!preview || self.preview_rectangle)
            && self.callable.pause && self.callable.seek
            && self.certified.pause && self.certified.seek
            && self.callable.supports(requirements)
            && self.certified.supports(requirements)
    }
}

/// What Standard, this app's own direct play, decided about an original file: the plan's own verdicts,
/// carried to the qualifier so that no qualifier re-derives a codec, Dolby Vision or container rule of
/// its own. The TV's pipeline plays what the file says and never reports a wrong picture or a silent
/// track as a failure, so an original Standard would not direct-play (the server converts it) is never
/// one a viewer's opt-in may hand to URI. These four verdicts are everything the gate asks: it is NOT
/// the server's own direct-play decision (the Media Decision Engine round trip), which the URI offer is
/// made before, so a file that clears all four is offered even where the server would still convert it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StandardDirectPlay {
    /// `video_direct_plays` against the attempt's caps: the codec, the raster bound and the frozen
    /// Dolby Vision decision's presentation (a refused one fails it).
    pub video: bool,
    /// The frozen Dolby Vision decision (`frozen_dv_decision`: the set's own answer through the
    /// profile's setting) admits this source's layering: it has none, or it plays as a base layer or as a
    /// declared stream. A Profile 5 file on a set without Dolby Vision is the refusal.
    pub dolby_vision: bool,
    /// The selected audio track plays on the SET'S OWN decoder (`audio_natively_plays_with`: the
    /// table's row, the Load's channel and rate limits, the direct-play mode). Standard may also decode
    /// a codec in the app, which the TV's pipeline cannot, so that is not enough here.
    pub audio: bool,
    /// The Part's container is one Standard streams (`part_is_streamable`: .mkv, .mp4, .m4v). Any other
    /// (.avi, .mov, .ts, ...) Standard hands to the server to remux into progressive MKV, which an
    /// original handed to the TV's pipeline whole would skip.
    pub container: bool,
}

impl StandardDirectPlay {
    /// Standard would direct-play this original (as far as the plan's own verdicts go, not the server's
    /// decision), and the TV's own pipeline can show and play it.
    pub fn plays(self) -> bool { self.first_gap().is_none() }

    /// The first verdict that says no, in the order the fields are declared (video, Dolby Vision,
    /// audio, container); `None` when [`plays`](Self::plays).
    pub fn first_gap(self) -> Option<UriDecline> {
        if !self.video { Some(UriDecline::StandardVideo) }
        else if !self.dolby_vision { Some(UriDecline::StandardDolbyVision) }
        else if !self.audio { Some(UriDecline::StandardAudio) }
        else if !self.container { Some(UriDecline::StandardContainer) }
        else { None }
    }
}

/// **Why the Experimental engine did not take a play's original file**, one variant per distinct
/// refusal on the way to a URI source: the guards in the plan before a request exists, the checks of
/// [`ResolvePort::qualify`] and the qualifier's own. It is a local diagnostic: the plan records it and
/// the event log says it in one line, `uri: original declined (<reason>)` with the
/// [`log_name`](Self::log_name) as the reason, when the viewer opted in and the play stayed on Standard. Nothing here changes a route: the
/// [`Verdict`] kind (Known, Unknown, Refused) alone decides, as it did before the reason existed, and a
/// reason names the FIRST check that failed, in the order the code tests them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UriDecline {
    /// The resolve has no URI port (a build without the URI engine, or a boot whose URI driver
    /// install was refused).
    NoPort,
    /// The play's backend policy no longer offers a URI original: this play already tried one, or it
    /// was refused or excluded.
    NotOffered,
    /// The viewer's audio or subtitle pick names a stream the Part does not list.
    TrackPickUnresolved,
    /// The playback speed is not 1x.
    Rate,
    /// The quality ceiling (a fixed rung the file exceeds or has no measured rate for) or Settings >
    /// Direct play rules the original file out.
    QualityOrDirectPlay,
    /// The link is a Plex relay, which never carries an original.
    Relay,
    /// Quality is Auto and the link is not known to be local, so Auto would stream it adaptively.
    AutoOnNonLocalLink,
    /// An audio enhancement (Boost Dialog, Normalize Loudness) is on: the server re-encodes the audio.
    AudioEnhancement,
    /// A subtitle is burned into the picture.
    SubtitleBurn,
    /// The selected subtitle is an external file the app draws itself.
    ClientSubtitle,
    /// The file has subtitle streams and none is selected: the TV's pipeline has no way to switch the
    /// default one off.
    SubtitleOff,
    /// The file carries Dolby Vision and Settings > Dolby Vision is Off.
    DolbyVisionOff,
    /// A trailer preview, which the exported window cannot place.
    Preview,
    /// The Part has no known duration or no URL.
    NoDurationOrUrl,
    /// The qualifier declined the file without saying more (a foreign or developer qualifier).
    NotCertified,
    /// The engine is not set to Experimental for this viewer.
    NotOptedIn,
    /// Standard would not direct-play the video (codec or size the set cannot show).
    StandardVideo,
    /// Standard would not direct-play the Dolby Vision layering.
    StandardDolbyVision,
    /// Standard would not direct-play the selected audio track on the set's own decoder.
    StandardAudio,
    /// Standard hands the Part's container to the server to remux.
    StandardContainer,
    /// The file has several audio tracks and the exported window's service can order them
    /// differently from Plex.
    ManyAudioTracksExported,
    /// The qualifier's certificate did not match this play (another selection, source, Dolby Vision
    /// record or track map) or had expired.
    CertificateMismatch,
    /// A subtitle is selected and the certificate does not carry it: the qualifier certified no render
    /// of that subtitle (the product carrier certifies none, so today a selected embedded subtitle
    /// always ends here).
    SubtitleNotCarried,
    /// The display path has no evidence on this firmware (the exported window with a real canvas, or
    /// the ACB plane, with the controls a URL carrier needs callable).
    NoDisplayEvidence,
}

impl UriDecline {
    /// The reason as a short phrase a tester can act on, for the decline log line. No parentheses: the
    /// line wraps it in one.
    pub fn log_name(self) -> &'static str {
        match self {
            Self::NoPort => "no URI engine in this build or boot",
            Self::NotOffered => "the URI original was already tried or is not offered for this play",
            Self::TrackPickUnresolved => "the picked audio or subtitle track is not in this file",
            Self::Rate => "playback speed is not 1x",
            Self::QualityOrDirectPlay =>
                "the quality ceiling or Direct Play setting rules out the original; set quality to Original and Direct Play to Auto",
            Self::Relay => "the link is a Plex relay",
            Self::AutoOnNonLocalLink =>
                "quality Auto on a link that is not local; set quality to Original",
            Self::AudioEnhancement => "an audio enhancement is on",
            Self::SubtitleBurn => "a subtitle is burned in",
            Self::ClientSubtitle => "the selected subtitle is an external file",
            Self::SubtitleOff => "the file has subtitle streams and none is selected; subtitle streams keep a play in Standard today",
            Self::DolbyVisionOff => "the file has Dolby Vision and the Dolby Vision setting is Off",
            Self::Preview => "a trailer preview",
            Self::NoDurationOrUrl => "no duration or URL for the file",
            Self::NotCertified => "the qualifier did not certify the file",
            Self::NotOptedIn => "Playback engine is not Experimental",
            Self::StandardVideo => "Standard would not direct-play the video",
            Self::StandardDolbyVision => "Standard would not direct-play the Dolby Vision",
            Self::StandardAudio => "Standard would not direct-play the audio",
            Self::StandardContainer => "Standard would not direct-play the container",
            Self::ManyAudioTracksExported => "several audio tracks on the exported window",
            Self::CertificateMismatch => "the certified source did not match this play",
            Self::SubtitleNotCarried =>
                "subtitle streams keep a play in Standard today; try a file without subtitle streams",
            Self::NoDisplayEvidence => "no exported-window evidence for this firmware",
        }
    }
}

/// URI source facts gathered from the selected PMS Part before Feed picks a sibling or encoder.
/// The Part URL and stream identifiers are intentionally absent from Debug/log output.
pub struct OriginalRequest {
    pub selection: SourceSelection,
    /// Where `selection.server`'s client dials: the scheme, host and port the registry holds for it,
    /// exactly as the connection was registered (a dev-token boot registers its primary by ADDRESS
    /// ONLY, so this is the one thing about that server that is always known). `selection.server` is
    /// only a process-local registry slot, so a qualifier that must tell one SERVER from another (a
    /// developer pin) compares this. Like the URL it stays out of Debug/log output: it is a
    /// household address.
    pub server_origin: plx_plex::plex::Origin,
    pub source_url: String,
    pub duration_ms: Option<u64>,
    pub video_codec: String,
    /// Exact selected-source layering; a positive DV verdict must certify this record.
    pub dolby_vision: plx_data::metadata::Dovi,
    pub dolby_vision_mode: plx_plex::plex::session::DolbyVisionMode,
    /// The viewer turned on Settings > Playback > Playback engine (`UriEngine::On`). That choice is
    /// the viewer's own qualification of a URI source for their own playback and nobody else's: a
    /// qualifier answers for it, never without it, and never past [`standard`](Self::standard).
    pub uri_opt_in: bool,
    /// Whether Standard would direct-play this original (the plan's video, Dolby Vision, audio and
    /// container verdicts, not the qualifier's and not the server's decision). The viewer's opt-in
    /// carries only an original it says yes to.
    pub standard: StandardDirectPlay,
    pub audio_codec: String,
    /// The selected audio stream's 0-based position among the Part's audio streams, in container
    /// order ([`plx_data::metadata::audio_ordinal`], the numbering the Feed demuxer's nth-of-type
    /// selection already uses); `None` when no audio stream is selected or the Part lists none.
    ///
    /// HYPOTHESIS, unmeasured on the 4.5 service: that this position is also the native track
    /// index the service names in `audioInfo.track` and takes in `selectTrack`. A qualifier may map
    /// it to a [`NativeTrackIndex`] only for a source it knows lists its audio in the Part's order;
    /// `selection.audio_sid` stays the stream's identity, this is only where it sits.
    pub audio_ordinal: Option<i64>,
    /// The exported-window service can enumerate multiple tracks in a different order from PMS.
    pub has_multiple_audio_tracks: bool,
    /// Filled by ResolvePort from the composed backend before calling the qualifier.
    pub binding: Binding,
    pub subtitle_source: Option<SourceSubtitle>,
    /// Off is an actual viewer choice, distinct from no subtitle streams being present.
    /// The current URI driver has no native disable operation.
    pub subtitle_choice: TrackChoice,
    pub source_kbps: Option<u32>,
    pub raster: Option<(u32, u32)>,
    pub requested_rate_permille: i32,
    pub resume_ns: i64,
    pub quality_allows_original: bool,
    pub link_allows_original: bool,
    pub auto_allows_original: bool,
    pub requires_subtitle_burn: bool,
    pub requires_client_subtitle: bool,
    pub preview: bool,
}

pub struct KnownSource {
    pub selection: SourceSelection,
    pub source: UriSource,
    pub requirements: Requirements,
    /// Source-specific DV certificate. `None` is enough only when PMS reports no DV.
    pub certified_dolby_vision: Option<plx_data::metadata::Dovi>,
    pub certified_subtitle_source: Option<SourceSubtitle>,
    pub expires_at: Instant,
}

/// A qualifier's answer for an original: the certificate, or the reason there is none. `Unknown` and
/// `Refused` route identically to a caller that only asks for a source; the reason is the diagnostic.
pub enum Verdict { Known(KnownSource), Unknown(UriDecline), Refused(UriDecline) }

/// The PMS output selected by a completed, accepted decision. These are source recipes, not
/// guesses from a Feed codec declaration or a URL suffix.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PmsRecipe { Remux, Encode, Hls }

/// Finite eligibility captured by the common route owner before an off-frame resolve. An
/// expired or refused Original never authorizes itself again, while an unspent PMS recipe can
/// still be certified independently. The worker reads only this value, never the live catalog.
#[derive(Clone, Copy)]
pub struct UriOfferEligibility {
    pub original: bool,
    pub remux: bool,
    pub encode: bool,
    pub hls: bool,
}

impl UriOfferEligibility {
    pub const ALL: Self = Self { original: true, remux: true, encode: true, hls: true };
    pub fn any_pms(self) -> bool { self.remux || self.encode || self.hls }
    pub fn permits(self, recipe: PmsRecipe) -> bool {
        match recipe {
            PmsRecipe::Remux => self.remux,
            PmsRecipe::Encode => self.encode,
            PmsRecipe::Hls => self.hls,
        }
    }
}

/// A URI driver's clock and seek values currently use content-absolute milliseconds. A PMS
/// source with a relative or unknown time base cannot use that driver without a mapping port.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PmsTimeDomain { ContentAbsolute, Unknown }

/// Exactly one adaptive owner. A native master is controlled by the platform; a fixed PMS
/// variant is controlled by the app's route owner and must not start Feed's ABR worker.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum HlsController { NativeMaster, AppFixed }

/// Evidence from reading the actual PMS master response, bound to its exact locator and
/// resource. A `.m3u8` suffix or a PMS delivery label supplies none of these facts.
pub struct HlsMasterEvidence {
    source_url: String,
    resource_session: String,
    variant_count: u16,
}

impl HlsMasterEvidence {
    /// Parse the actual bounded response from the accepted PMS locator. The qualifier supplies
    /// the final response URL and must read it off frame; a delivery label or URL suffix cannot
    /// construct this receipt. Feed's fixed-HLS parser accepts exactly one variant, so it cannot
    /// certify a native adaptive master and is unavailable in URI-only builds.
    pub fn from_response(source_url: String, resource_session: String, body: &[u8]) -> Option<Self> {
        if source_url.is_empty() || resource_session.is_empty() || body.len() > 131_072 {
            return None;
        }
        let text = std::str::from_utf8(body).ok()?;
        let mut lines = text.lines().map(str::trim);
        if lines.next()? != "#EXTM3U" { return None; }
        let mut waiting_for_uri = false;
        let mut variant_count = 0u16;
        for line in lines {
            if line.is_empty() { continue; }
            if line.starts_with("#EXTINF:") || line.starts_with("#EXT-X-TARGETDURATION:")
                || line.starts_with("#EXT-X-MEDIA-SEQUENCE:") || line == "#EXT-X-ENDLIST" {
                return None;
            }
            if waiting_for_uri {
                if line.starts_with('#') || line.chars().any(char::is_whitespace) { return None; }
                variant_count = variant_count.checked_add(1)?;
                waiting_for_uri = false;
                continue;
            }
            if let Some(attrs) = line.strip_prefix("#EXT-X-STREAM-INF:") {
                let bandwidth = attrs.split(',').find_map(|attr|
                    attr.trim().strip_prefix("BANDWIDTH=").and_then(|v| v.parse::<u64>().ok()));
                if bandwidth.is_none_or(|value| value == 0) { return None; }
                waiting_for_uri = true;
            } else if !line.starts_with('#') {
                return None;
            }
        }
        (!waiting_for_uri && variant_count >= 2).then_some(Self {
            source_url, resource_session, variant_count,
        })
    }
}

/// Owned IO facts captured after PMS accepted its decision. The URL and resource identity are
/// never included in Debug output; the resolver retains their exact ownership on every exit.
pub struct PmsRequest {
    pub selection: SourceSelection,
    pub source_url: String,
    pub resource_session: String,
    pub duration_ms: Option<u64>,
    pub resume_ns: i64,
    pub recipe: PmsRecipe,
    /// Auto's HLS route ordinarily has an app ABR worker in Feed. A URI master may replace that
    /// worker only after the qualifier certifies platform control of the actual output.
    pub adaptive: bool,
    pub video_codec: String,
    pub audio_output: PmsAudioOutput,
    pub source_dovi: plx_data::metadata::Dovi,
    pub output_dovi: Option<plx_data::metadata::Dovi>,
    pub dolby_vision_mode: plx_plex::plex::session::DolbyVisionMode,
    pub applied_audio: plx_plex::plex::AudioEnhancements,
    pub subtitle_choice: TrackChoice,
    pub requires_client_subtitle: bool,
    pub burned_subtitle: bool,
    /// The accepted `/decision` named this exact source subtitle as a server burn.
    pub accepted_burn_sid: Option<i64>,
    pub requested_rate_permille: i32,
    pub preview: bool,
}

/// Source-specific evidence from the IO qualifier. Its exact resource and source selection
/// prevent a certificate for an earlier `/decision` from authorizing the next PMS output.
pub struct PmsCertificate {
    pub selection: SourceSelection,
    pub source_url: String,
    pub resource_session: String,
    pub recipe: PmsRecipe,
    pub requirements: Requirements,
    pub native_tracks: Vec<NativeTrackIndex>,
    pub applied_audio: plx_plex::plex::AudioEnhancements,
    pub video_codec: String,
    pub audio_output: PmsAudioOutput,
    pub output_dovi: Option<plx_data::metadata::Dovi>,
    /// Independent native/source proof for the exact output DV shape. `None` means no proof;
    /// `NoDv` is an affirmative probe, not an inference from omitted `/decision` keys.
    pub certified_dovi_output: Option<CertifiedDoviOutput>,
    pub accepted_burn_sid: Option<i64>,
    pub time_domain: PmsTimeDomain,
    pub hls_controller: Option<HlsController>,
    pub hls_master: Option<HlsMasterEvidence>,
    pub release_certified: bool,
    pub expires_at: Instant,
}

pub struct KnownPmsSource {
    pub selection: SourceSelection,
    pub recipe: PmsRecipe,
    pub source: UriSource,
    pub requirements: Requirements,
    pub applied_audio: plx_plex::plex::AudioEnhancements,
    pub resource_session: String,
    pub expires_at: Instant,
}

/// Audio actually registered by the accepted PMS output. `Silent` is constructed only when
/// source metadata has no audio and the accepted output has no audio stream either.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PmsAudioOutput { Codec(String), Silent }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CertifiedDoviOutput { NoDv, Exact(plx_data::metadata::Dovi) }

pub enum PmsVerdict { Known(PmsCertificate), Unknown, Refused }

/// Local diagnostic reason for declining a PMS URI output. These are bounded route facts, not
/// firmware claims or transmitted telemetry; the generic localized open sentence is unchanged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PmsDecline {
    UnsupportedIntent,
    MissingSourceFacts,
    UnavailableCapability,
    MissingOutputEvidence,
    UnknownCertificate,
    ProviderRefused,
    AppFixedControllerUnavailable,
    UnmappedClock,
    NoReleaseProof,
    StaleOrMismatched,
    UnexecutableSelection,
}

/// The caller supplies the actual Part URL and selected source tracks. An implementation may
/// perform bounded IO only with the worker's OffFrame proof; it must not infer a URI certificate
/// from Feed's codec table, MDE answer or native ACK. [`OriginalRequest::standard`] is no certificate
/// either: it is Standard's own verdict, which can only narrow what a qualifier carries.
pub trait OriginalQualifier: Send + Sync + std::panic::RefUnwindSafe {
    fn qualify(&self, off: &OffFrame, request: OriginalRequest) -> Verdict;

    /// A separate certificate for the actual PMS output. Existing product providers decline
    /// until firmware, source time mapping, controls and release have all been established.
    fn qualify_pms(&self, _off: &OffFrame, _request: PmsRequest) -> PmsVerdict {
        PmsVerdict::Unknown
    }
}

#[derive(Clone)]
pub struct ResolvePort {
    evidence: ProductEvidence,
    qualifier: Arc<dyn OriginalQualifier>,
}

impl ResolvePort {
    pub fn new(evidence: ProductEvidence, qualifier: Arc<dyn OriginalQualifier>) -> Self {
        Self { evidence, qualifier }
    }

    pub fn evidence(&self) -> ProductEvidence { self.evidence }

    /// Whether even attempting a PMS candidate could produce a URI offer. This is only a
    /// preflight cost gate; it never qualifies a source or promises release.
    pub fn could_qualify_pms(&self) -> bool {
        let caps = self.evidence.certified;
        self.evidence.display_path_offerable()
            && self.evidence.callable.pause && self.evidence.callable.seek
            && caps.pause && caps.seek
            && ((self.evidence.callable.progressive_uri && caps.progressive_uri)
                || (self.evidence.callable.hls_uri && caps.hls_uri))
    }

    pub fn qualify_pms(&self, off: &OffFrame, request: PmsRequest)
        -> Result<KnownPmsSource, PmsDecline> {
        if request.preview || request.requested_rate_permille != plx_playback_contract::RATE_NORMAL
            || request.requires_client_subtitle
            || request.subtitle_choice == TrackChoice::Off {
            return Err(PmsDecline::UnsupportedIntent);
        }
        if request.burned_subtitle && (request.selection.subtitle_sid <= 0
            || request.accepted_burn_sid != Some(request.selection.subtitle_sid)
            || request.recipe == PmsRecipe::Remux) {
            return Err(PmsDecline::MissingOutputEvidence);
        }
        // Off constrains the accepted output even when the original source's metadata omitted
        // DOVI. A separately certified no-DV PMS output can still qualify below.
        if request.dolby_vision_mode == plx_plex::plex::session::DolbyVisionMode::Off
            && request.output_dovi.is_some_and(|d| d.present) {
            return Err(PmsDecline::UnsupportedIntent);
        }
        if request.resource_session.is_empty() || request.source_url.is_empty()
            || request.video_codec.is_empty()
            || matches!(&request.audio_output, PmsAudioOutput::Codec(codec) if codec.is_empty())
            || request.applied_audio != request.selection.audio_contract
            || request.resume_ns < 0 || request.duration_ms.is_none_or(|ms| ms == 0
                || ms.checked_mul(1_000_000).is_none_or(|ns|
                    ns > i64::MAX as u64 || request.resume_ns as u64 > ns)) {
            return Err(PmsDecline::MissingSourceFacts);
        }
        if !self.could_qualify_pms() {
            return Err(PmsDecline::UnavailableCapability);
        }
        let selection = request.selection;
        let recipe = request.recipe;
        let adaptive = request.adaptive;
        let session = request.resource_session.clone();
        let url = request.source_url.clone();
        let audio = request.applied_audio;
        let video_codec = request.video_codec.clone();
        let audio_output = request.audio_output.clone();
        let source_dovi = request.source_dovi;
        let output_dovi = request.output_dovi;
        let accepted_burn_sid = request.accepted_burn_sid;
        let burned_subtitle = request.burned_subtitle;
        let source_kind = match recipe {
            PmsRecipe::Remux | PmsRecipe::Encode => SourceKind::ProgressiveUri,
            PmsRecipe::Hls => SourceKind::HlsUri,
        };
        let cert = match self.qualifier.qualify_pms(off, request) {
            PmsVerdict::Known(cert) => cert,
            PmsVerdict::Unknown => return Err(PmsDecline::UnknownCertificate),
            PmsVerdict::Refused => return Err(PmsDecline::ProviderRefused),
        };
        let requirements = cert.requirements;
        let dv_output_proven = match (output_dovi, cert.certified_dovi_output) {
            (Some(expected), Some(CertifiedDoviOutput::Exact(actual))) => expected == actual,
            (None, Some(CertifiedDoviOutput::NoDv)) => true,
            (None, None) => !source_dovi.present,
            _ => false,
        };
        if !dv_output_proven { return Err(PmsDecline::MissingOutputEvidence); }
        if recipe == PmsRecipe::Hls && cert.hls_controller == Some(HlsController::AppFixed) {
            return Err(PmsDecline::AppFixedControllerUnavailable);
        }
        if cert.time_domain != PmsTimeDomain::ContentAbsolute {
            return Err(PmsDecline::UnmappedClock);
        }
        if !cert.release_certified {
            return Err(PmsDecline::NoReleaseProof);
        }
        if cert.selection != selection || cert.source_url != url
            || cert.resource_session != session || cert.recipe != recipe
            || cert.applied_audio != audio || cert.video_codec != video_codec
            || cert.audio_output != audio_output || cert.output_dovi != output_dovi
            || cert.accepted_burn_sid != accepted_burn_sid
            || cert.expires_at <= Instant::now()
            || requirements.source != source_kind
            || requirements.video != Fidelity::Compatible
            || requirements.audio != Fidelity::Compatible
            || match recipe {
                PmsRecipe::Hls if adaptive =>
                    cert.hls_controller != Some(HlsController::NativeMaster)
                        || cert.hls_master.as_ref().is_none_or(|master|
                            master.source_url != url || master.resource_session != session
                                || master.variant_count < 2),
                // The common URI app-controller port and observations for a fixed variant do
                // not exist yet. Do not quietly borrow Feed's worker or freeze an Auto rung.
                PmsRecipe::Hls => true,
                PmsRecipe::Remux | PmsRecipe::Encode =>
                    cert.hls_controller.is_some() || cert.hls_master.is_some(),
            }
            || !self.evidence.permits(requirements, false) {
            return Err(PmsDecline::StaleOrMismatched);
        }
        if audio_output == PmsAudioOutput::Silent && cert.native_tracks.iter().any(|track|
            track.selection.kind == TrackKind::Audio) {
            return Err(PmsDecline::UnexecutableSelection);
        }
        // Construct the locator from this exact accepted PMS decision. A qualifier cannot
        // replace it with a different URL after certifying the resource and stream map.
        let source = UriSource::new(url, cert.native_tracks);
        if !source.can_supply(requirements)
            || (selection.audio_sid > 0 && requirements.audio_track
                != TrackChoice::Selected(selection.audio_sid as u64))
            || (audio_output == PmsAudioOutput::Silent
                && (selection.audio_sid > 0 || requirements.audio_track != TrackChoice::Unspecified))
            || (selection.subtitle_sid > 0 && requirements.subtitle_track
                != if burned_subtitle { TrackChoice::Unspecified }
                   else { TrackChoice::Selected(selection.subtitle_sid as u64) }) {
            return Err(PmsDecline::UnexecutableSelection);
        }
        Ok(KnownPmsSource { selection, recipe, source, requirements,
            applied_audio: audio, resource_session: session, expires_at: cert.expires_at })
    }

    pub fn qualify(&self, off: &OffFrame, mut request: OriginalRequest) -> Verdict {
        // Evidence is certified for everyone only as far as the product has proven it; a viewer
        // who opted in has also certified the URL carrier for their own playback.
        let evidence = if request.uri_opt_in { self.evidence.opted_in() } else { self.evidence };
        // The first check that says no names the reason, in this order.
        if let Some(why) = Self::refusal(&request, &evidence) {
            return Verdict::Refused(why);
        }
        if request.duration_ms.is_none() || request.source_url.is_empty() {
            return Verdict::Unknown(UriDecline::NoDurationOrUrl);
        }
        let preview = request.preview;
        let selected = request.selection;
        let dovi = request.dolby_vision;
        let subtitle = request.subtitle_source.clone();
        let subtitle_choice = request.subtitle_choice;
        request.binding = evidence.binding;
        match self.qualifier.qualify(off, request) {
            Verdict::Known(known) => {
                // The certificate is judged against this play first (its selection, then the subtitle it
                // must carry), then the display path it would use: separate reasons, so a tester knows
                // what to change and what to report.
                if let Some(gap) = known.certificate_gap(selected, dovi, subtitle.as_ref(), subtitle_choice) {
                    Verdict::Unknown(gap)
                } else if !evidence.permits(known.requirements, preview) {
                    Verdict::Unknown(UriDecline::NoDisplayEvidence)
                } else {
                    Verdict::Known(known)
                }
            }
            other => other,
        }
    }

    /// The first viewer or route refusal an original meets before any qualifier is asked, in the order
    /// the checks have always run. A refusal is final for this play (`Verdict::Refused`).
    fn refusal(request: &OriginalRequest, evidence: &ProductEvidence) -> Option<UriDecline> {
        if request.requested_rate_permille != plx_playback_contract::RATE_NORMAL { Some(UriDecline::Rate) }
        else if !request.quality_allows_original { Some(UriDecline::QualityOrDirectPlay) }
        else if !request.link_allows_original { Some(UriDecline::Relay) }
        else if !request.auto_allows_original { Some(UriDecline::AutoOnNonLocalLink) }
        else if request.selection.audio_contract.any() { Some(UriDecline::AudioEnhancement) }
        else if request.requires_subtitle_burn { Some(UriDecline::SubtitleBurn) }
        else if request.requires_client_subtitle { Some(UriDecline::ClientSubtitle) }
        else if request.subtitle_choice == TrackChoice::Off { Some(UriDecline::SubtitleOff) }
        else if request.dolby_vision.present
            && request.dolby_vision_mode == plx_plex::plex::session::DolbyVisionMode::Off {
            Some(UriDecline::DolbyVisionOff)
        } else if request.preview && !evidence.preview_rectangle { Some(UriDecline::Preview) }
        else { None }
    }
}

impl KnownSource {
    /// Where this certificate is not the one the play asked for, or `None` when it is. First the rest of
    /// the certificate ([`UriDecline::CertificateMismatch`]): the same selection, an exact progressive
    /// source that can supply the audio track it requires, the Dolby Vision record, the audio track the
    /// play selected, and not yet expired. Then the subtitle the play selected, if any
    /// ([`UriDecline::SubtitleNotCarried`]): the source must map its native track and the certificate
    /// must carry the embedded subtitle's render. Both groups together are the one boolean this check
    /// has always been; the split only names which group failed.
    fn certificate_gap(&self, selected: SourceSelection, dovi: plx_data::metadata::Dovi,
        subtitle: Option<&SourceSubtitle>, subtitle_choice: TrackChoice) -> Option<UriDecline> {
        let subtitle_selected = selected.subtitle_sid > 0;
        // A selected subtitle's native track is the subtitle group's to answer for, so the rest
        // is asked of the source without it.
        let rest = Requirements { subtitle_track: if subtitle_selected { TrackChoice::Unspecified }
            else { self.requirements.subtitle_track }, ..self.requirements };
        if !(self.selection == selected
            && self.requirements.source == SourceKind::ProgressiveUri
            && self.requirements.video == Fidelity::Exact
            && self.requirements.audio == Fidelity::Exact
            && self.source.can_supply(rest)
            && (!dovi.present || self.certified_dolby_vision == Some(dovi))
            && self.requirements.subtitle_track == subtitle_choice
            && (selected.audio_sid <= 0 || self.requirements.audio_track
                == TrackChoice::Selected(selected.audio_sid as u64))
            && self.expires_at > Instant::now()) {
            Some(UriDecline::CertificateMismatch)
        } else if subtitle_selected && !(self.source.can_supply(self.requirements)
            && self.requirements.subtitle_track == TrackChoice::Selected(selected.subtitle_sid as u64)
            && subtitle.is_some_and(|sub| sub.embedded)
            && self.certified_subtitle_source.as_ref() == subtitle) {
            Some(UriDecline::SubtitleNotCarried)
        } else {
            None
        }
    }
}

/// How long a verdict that qualifies a URL source stays good: it must outlive the plan landing and
/// the staged driver source (the driver's own TTL is 30 s).
pub const SOURCE_VERDICT_TTL: Duration = Duration::from_secs(30);

impl KnownSource {
    /// The certificate of a URL carrier: `request`'s own Part, carried by `url` as a progressive
    /// source, exact video and audio. The Part lists its audio in order, so the selected stream's
    /// position in it is its native index ([`OriginalRequest::audio_ordinal`], a hypothesis the 4.5
    /// service has not yet confirmed). A stream whose position the plan could not name maps to
    /// nothing: the source then cannot supply the selection and the route plays the Feed, instead of
    /// the service silently staying on track 0. No subtitle source is certified, and a Dolby Vision
    /// record is certified only where the frozen decision admitted it ([`StandardDirectPlay::dolby_vision`]).
    pub fn url_carrier(request: &OriginalRequest, url: String, ttl: Duration) -> KnownSource {
        let selected_audio = u64::try_from(request.selection.audio_sid).ok().filter(|sid| *sid > 0);
        let tracks = selected_audio.zip(request.audio_ordinal).map(|(id, index)| NativeTrackIndex {
            selection: TrackSelection { kind: TrackKind::Audio, id }, index,
        });
        KnownSource {
            selection: request.selection,
            source: UriSource::new(url, tracks.into_iter().collect()),
            requirements: Requirements { source: SourceKind::ProgressiveUri,
                video: Fidelity::Exact, audio: Fidelity::Exact,
                audio_track: selected_audio.map_or(TrackChoice::Unspecified, TrackChoice::Selected),
                subtitle_track: request.subtitle_choice },
            // Only the layering the frozen Dolby Vision decision admitted: a refused one (Profile 5 on
            // a set without Dolby Vision) is no source this carrier can show correctly, and the DV
            // check of `ResolvePort::qualify` then declines it.
            certified_dolby_vision: (request.dolby_vision.present && request.standard.dolby_vision)
                .then_some(request.dolby_vision),
            certified_subtitle_source: None,
            expires_at: crate::player::adapter::uri_source_expiry_after(ttl),
        }
    }
}

/// Native product source evidence is unqualified for everyone but a viewer who turned on the URI
/// engine in Settings ([`OriginalRequest::uri_opt_in`]): that choice is their own qualification of
/// the URL carrier, so the answer is Unknown without it. Even then it covers only an original Standard
/// would direct-play ([`OriginalRequest::standard`]: the plan's own video, Dolby Vision, audio and
/// container verdicts, which this qualifier reads and never re-derives; not the server's own decision,
/// which the offer is made before): every other file plays exactly as it does in Standard, where the
/// server converts what the set cannot show or decode and remuxes a container Standard does not stream. The same port is run by
/// every real resolve and can consume a future certified provider without changing route policy.
pub struct ProductQualifier;
impl OriginalQualifier for ProductQualifier {
    fn qualify(&self, _off: &OffFrame, request: OriginalRequest) -> Verdict {
        if !request.uri_opt_in { return Verdict::Unknown(UriDecline::NotOptedIn); }
        if let Some(gap) = request.standard.first_gap() { return Verdict::Unknown(gap); }
        // The product carrier assumes Plex order. Explicitly certified providers may supply
        // their own track map, so this restriction belongs to this qualifier.
        if request.has_multiple_audio_tracks && matches!(request.binding, Binding::Exported { .. }) {
            return Verdict::Refused(UriDecline::ManyAudioTracksExported);
        }
        let url = request.source_url.clone();
        Verdict::Known(KnownSource::url_carrier(&request, url, SOURCE_VERDICT_TTL))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plx_playback_contract::{TrackKind, TrackSelection};

    #[test]
    fn an_acb_binding_certifies_and_offers_nothing() {
        use plx_platform::tv::sink::VP_ACB;
        // Configuring the executor with it certifies and offers nothing.
        let evidence = ProductEvidence::unqualified_on(VP_ACB, Binding::Acb);
        assert!(!evidence.permits(Requirements { source: SourceKind::ProgressiveUri, video: Fidelity::Exact,
            audio: Fidelity::Exact, audio_track: TrackChoice::Unspecified, subtitle_track: TrackChoice::Unspecified }, false));
    }

    const PROGRESSIVE_EXACT: Requirements = Requirements { source: SourceKind::ProgressiveUri,
        video: Fidelity::Exact, audio: Fidelity::Exact, audio_track: TrackChoice::Unspecified,
        subtitle_track: TrackChoice::Unspecified };

    fn uri_caps(on: bool) -> Capabilities {
        Capabilities { progressive_uri: on, hls_uri: false, packet_feed: false, pause: on,
            seek: on, audio_track: on, subtitle_track: false, audio_off: false, subtitle_off: false }
    }

    fn evidence_with(display_exported: bool, binding: Binding, certified: Capabilities)
        -> ProductEvidence {
        ProductEvidence { display_exported, binding, callable: uri_caps(true), certified,
            preview_rectangle: false }
    }

    #[test]
    fn acb_binding_is_offerable_only_with_certified_capabilities() {
        // The ACB display path is an offerable path beside the exported canvas: with the source
        // class, pause and seek certified there is an offer, and `could_qualify_pms` agrees.
        let certified = evidence_with(false, Binding::Acb, uri_caps(true));
        assert!(certified.permits(PROGRESSIVE_EXACT, false));
        let port = ResolvePort::new(certified, Arc::new(ProductQualifier));
        assert!(port.could_qualify_pms());
        // Nothing certified: the display path alone offers nothing, so production stays locked.
        let uncertified = evidence_with(false, Binding::Acb, uri_caps(false));
        assert!(!uncertified.permits(PROGRESSIVE_EXACT, false));
        assert!(!ResolvePort::new(uncertified, Arc::new(ProductQualifier)).could_qualify_pms());
        // The preview rectangle is its own certificate, ACB or not.
        assert!(!certified.permits(PROGRESSIVE_EXACT, true));
    }

    #[test]
    fn only_an_exported_canvas_or_acb_is_an_offerable_display_path() {
        let all = uri_caps(true);
        let exported = Binding::Exported { canvas: (1920, 1080) };
        assert!(evidence_with(true, exported, all).permits(PROGRESSIVE_EXACT, false));
        // An exported canvas needs the display mode that exports it and a real canvas.
        assert!(!evidence_with(false, exported, all).permits(PROGRESSIVE_EXACT, false));
        assert!(!evidence_with(true, Binding::Exported { canvas: (0, 1080) }, all)
            .permits(PROGRESSIVE_EXACT, false));
        for display_exported in [false, true] {
            let unbound = evidence_with(display_exported, Binding::Unbound, all);
            assert!(!unbound.permits(PROGRESSIVE_EXACT, false));
            assert!(!ResolvePort::new(unbound, Arc::new(ProductQualifier)).could_qualify_pms());
        }
    }

    #[test]
    fn the_production_evidence_certifies_nothing_on_any_binding() {
        use plx_platform::tv::sink::{VP_ACB, VP_EXPORTED};
        for (mode, binding) in [(VP_ACB, Binding::Acb), (VP_EXPORTED, Binding::Acb),
            (VP_EXPORTED, Binding::Exported { canvas: (1920, 1080) }), (VP_ACB, Binding::Unbound)] {
            let evidence = ProductEvidence::unqualified_on(mode, binding);
            assert!(!evidence.permits(PROGRESSIVE_EXACT, false));
            assert!(!ResolvePort::new(evidence, Arc::new(ProductQualifier)).could_qualify_pms());
            assert!(!evidence.certified.supports(PROGRESSIVE_EXACT));
        }
    }

    /// The composition's exported-window answer for a viewer who opted in: production evidence
    /// certifies nothing, and the opt-in certifies the URL carrier, so an exact progressive original
    /// is permitted through the exported window (and through nothing else: not an unbound executor,
    /// not a display that is not in the exported-window mode, not the preview rectangle).
    #[test]
    fn an_opted_in_viewer_is_permitted_exact_progressive_through_the_exported_window_only() {
        use plx_platform::tv::sink::{VP_ACB, VP_EXPORTED};
        let exported = Binding::Exported { canvas: (1920, 1080) };
        let production = ProductEvidence::unqualified_on(VP_EXPORTED, exported);
        assert!(!production.permits(PROGRESSIVE_EXACT, false), "without the opt-in nothing is certified");
        let opted_in = production.opted_in();
        assert!(opted_in.permits(PROGRESSIVE_EXACT, false));
        assert!(!opted_in.permits(PROGRESSIVE_EXACT, true), "the preview rectangle is its own certificate");
        assert!(!ProductEvidence::unqualified_on(VP_EXPORTED, Binding::Unbound).opted_in()
            .permits(PROGRESSIVE_EXACT, false), "an unbound executor offers nothing");
        assert!(!ProductEvidence::unqualified_on(VP_ACB, exported).opted_in()
            .permits(PROGRESSIVE_EXACT, false), "the exported window needs the display mode that exports it");
    }

    const MASTER: &[u8] = b"#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1000000\nlow/index.m3u8\n#EXT-X-STREAM-INF:BANDWIDTH=3000000\nhigh/index.m3u8\n";

    #[derive(Clone, Copy)]
    enum PmsCase { Valid, VerifiedDovi, Unknown, Refused, WrongSource, WrongSession,
        WrongOutputCodec, UnknownClock,
        NoRelease, Expired, FixedHls, FalseMaster }

    struct CertifiedPms(PmsCase);
    impl OriginalQualifier for CertifiedPms {
        fn qualify(&self, _: &OffFrame, _: OriginalRequest) -> Verdict { Verdict::Unknown(UriDecline::NotCertified) }

        fn qualify_pms(&self, _: &OffFrame, request: PmsRequest) -> PmsVerdict {
            if matches!(self.0, PmsCase::Unknown) { return PmsVerdict::Unknown; }
            if matches!(self.0, PmsCase::Refused) { return PmsVerdict::Refused; }
            let is_hls = request.recipe == PmsRecipe::Hls;
            let master = is_hls.then(|| HlsMasterEvidence::from_response(
                request.source_url.clone(), request.resource_session.clone(),
                if matches!(self.0, PmsCase::FalseMaster) {
                    &b"#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXTINF:4,\nsegment.ts\n"[..]
                } else { MASTER },
            )).flatten();
            PmsVerdict::Known(PmsCertificate {
                selection: request.selection,
                source_url: if matches!(self.0, PmsCase::WrongSource) {
                    "https://example.invalid/earlier-source".to_owned()
                } else { request.source_url },
                resource_session: if matches!(self.0, PmsCase::WrongSession) {
                    "earlier-transcode".to_owned()
                } else { request.resource_session },
                recipe: request.recipe,
                requirements: Requirements {
                    source: if is_hls { SourceKind::HlsUri } else { SourceKind::ProgressiveUri },
                    video: Fidelity::Compatible, audio: Fidelity::Compatible,
                    audio_track: if request.selection.audio_sid > 0 {
                        TrackChoice::Selected(42)
                    } else { TrackChoice::Unspecified },
                    subtitle_track: TrackChoice::Unspecified,
                },
                native_tracks: (request.selection.audio_sid > 0).then_some(NativeTrackIndex {
                    selection: TrackSelection { kind: TrackKind::Audio, id: 42 }, index: 1,
                }).into_iter().collect(),
                applied_audio: request.applied_audio,
                video_codec: if matches!(self.0, PmsCase::WrongOutputCodec) {
                    "hevc".to_owned()
                } else { request.video_codec },
                audio_output: request.audio_output,
                output_dovi: request.output_dovi,
                certified_dovi_output: matches!(self.0, PmsCase::VerifiedDovi).then(||
                    request.output_dovi.map_or(CertifiedDoviOutput::NoDv,
                        CertifiedDoviOutput::Exact)),
                accepted_burn_sid: request.accepted_burn_sid,
                time_domain: if matches!(self.0, PmsCase::UnknownClock) {
                    PmsTimeDomain::Unknown
                } else { PmsTimeDomain::ContentAbsolute },
                hls_controller: is_hls.then_some(if matches!(self.0, PmsCase::FixedHls) {
                    HlsController::AppFixed
                } else { HlsController::NativeMaster }),
                hls_master: master,
                release_certified: !matches!(self.0, PmsCase::NoRelease),
                expires_at: if matches!(self.0, PmsCase::Expired) {
                    Instant::now() - std::time::Duration::from_secs(1)
                } else { Instant::now() + std::time::Duration::from_secs(5) },
            })
        }
    }

    fn pms_request(recipe: PmsRecipe) -> PmsRequest {
        PmsRequest { selection: SourceSelection {
            server: plx_plex::plex::ServerId::from_raw(1), part_id: 4, audio_sid: 42,
            subtitle_sid: 0, audio_contract: plx_plex::plex::AudioEnhancements::NONE,
        }, source_url: if recipe == PmsRecipe::Hls {
            "https://example.invalid/session/master.m3u8"
        } else { "https://example.invalid/session/start.mkv" }.to_owned(),
            resource_session: "current-transcode".to_owned(), duration_ms: Some(60_000),
            resume_ns: 20_000_000_000, recipe, adaptive: recipe == PmsRecipe::Hls,
            video_codec: "h264".to_owned(),
            audio_output: PmsAudioOutput::Codec("aac".to_owned()),
            source_dovi: plx_data::metadata::Dovi::NONE, output_dovi: None,
            dolby_vision_mode: plx_plex::plex::session::DolbyVisionMode::Auto,
            applied_audio: plx_plex::plex::AudioEnhancements::NONE,
            subtitle_choice: TrackChoice::Unspecified, requires_client_subtitle: false,
            burned_subtitle: false, accepted_burn_sid: None,
            requested_rate_permille: plx_playback_contract::RATE_NORMAL,
            preview: false }
    }

    fn pms_port(case: PmsCase) -> ResolvePort {
        let capabilities = Capabilities { progressive_uri: true, hls_uri: true, packet_feed: false,
            pause: true, seek: true, audio_track: true, subtitle_track: true,
            audio_off: false, subtitle_off: false };
        ResolvePort::new(ProductEvidence { display_exported: true,
            binding: Binding::Exported { canvas: (1920, 1080) },
            callable: capabilities, certified: capabilities, preview_rectangle: false },
            Arc::new(CertifiedPms(case)))
    }

    #[test]
    fn pms_output_requires_exact_resource_clock_and_executable_source() {
        let off = OffFrame::for_test();
        for recipe in [PmsRecipe::Remux, PmsRecipe::Encode, PmsRecipe::Hls] {
            let source = pms_port(PmsCase::Valid).qualify_pms(&off, pms_request(recipe))
                .expect("certified current PMS output");
            assert!(source.recipe == recipe);
            assert_eq!(source.resource_session, "current-transcode");
            assert!(source.source.can_supply(source.requirements));
        }
        for (case, expected) in [
            (PmsCase::Unknown, PmsDecline::UnknownCertificate),
            (PmsCase::Refused, PmsDecline::ProviderRefused),
            (PmsCase::WrongSource, PmsDecline::StaleOrMismatched),
            (PmsCase::WrongSession, PmsDecline::StaleOrMismatched),
            (PmsCase::WrongOutputCodec, PmsDecline::StaleOrMismatched),
            (PmsCase::UnknownClock, PmsDecline::UnmappedClock),
            (PmsCase::NoRelease, PmsDecline::NoReleaseProof),
            (PmsCase::Expired, PmsDecline::StaleOrMismatched),
        ] {
            assert_eq!(pms_port(case).qualify_pms(&off, pms_request(PmsRecipe::Encode))
                .err(), Some(expected), "unproven PMS source cannot start");
        }
        let mut invalid = pms_request(PmsRecipe::Remux);
        invalid.duration_ms = None;
        assert_eq!(pms_port(PmsCase::Valid).qualify_pms(&off, invalid).err(),
            Some(PmsDecline::MissingSourceFacts));
        let mut invalid = pms_request(PmsRecipe::Remux);
        invalid.resume_ns = 61_000_000_000;
        assert_eq!(pms_port(PmsCase::Valid).qualify_pms(&off, invalid).err(),
            Some(PmsDecline::MissingSourceFacts));
        let mut unsupported = pms_request(PmsRecipe::Remux);
        unsupported.subtitle_choice = TrackChoice::Off;
        assert_eq!(pms_port(PmsCase::Valid).qualify_pms(&off, unsupported).err(),
            Some(PmsDecline::UnsupportedIntent));
    }

    #[test]
    fn native_master_needs_the_actual_multi_variant_response() {
        assert!(HlsMasterEvidence::from_response("master".into(), "resource".into(), MASTER).is_some());
        assert!(HlsMasterEvidence::from_response("master".into(), "resource".into(),
            b"#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1000000\none/index.m3u8\n").is_none());
        assert!(HlsMasterEvidence::from_response("master".into(), "resource".into(),
            b"#EXTM3U\n#EXT-X-TARGETDURATION:4\n#EXTINF:4,\nsegment.ts\n").is_none());
        let off = OffFrame::for_test();
        assert!(pms_port(PmsCase::FalseMaster).qualify_pms(&off, pms_request(PmsRecipe::Hls)).is_err());
        assert_eq!(pms_port(PmsCase::FixedHls).qualify_pms(&off,
            pms_request(PmsRecipe::Hls)).err(), Some(PmsDecline::AppFixedControllerUnavailable),
            "AppFixed has no common URI controller/observation actuator yet");
    }

    #[test]
    fn accepted_burn_needs_no_native_subtitle_selection() {
        let off = OffFrame::for_test();
        let mut request = pms_request(PmsRecipe::Encode);
        request.selection.subtitle_sid = 77;
        request.subtitle_choice = TrackChoice::Selected(77);
        request.burned_subtitle = true;
        request.accepted_burn_sid = Some(77);
        let source = pms_port(PmsCase::Valid).qualify_pms(&off, request)
            .expect("accepted server burn is carried in the video");
        assert_eq!(source.requirements.subtitle_track, TrackChoice::Unspecified);
        let mut missing = pms_request(PmsRecipe::Encode);
        missing.selection.subtitle_sid = 77;
        missing.subtitle_choice = TrackChoice::Selected(77);
        missing.burned_subtitle = true;
        assert_eq!(pms_port(PmsCase::Valid).qualify_pms(&off, missing).err(),
            Some(PmsDecline::MissingOutputEvidence));
    }

    #[test]
    fn copied_profile_seven_cannot_use_a_generic_pms_certificate() {
        let off = OffFrame::for_test();
        let mut request = pms_request(PmsRecipe::Remux);
        let p7 = plx_data::metadata::Dovi { present: true, profile: 7, bl_compat: 6,
            el_present: true, rpu_present: true, bl_present: true,
            ..plx_data::metadata::Dovi::NONE };
        request.source_dovi = p7;
        request.output_dovi = Some(p7);
        let mut verified = pms_request(PmsRecipe::Remux);
        verified.source_dovi = p7;
        verified.output_dovi = Some(p7);
        assert_eq!(pms_port(PmsCase::Valid).qualify_pms(&off, request).err(),
            Some(PmsDecline::MissingOutputEvidence),
            "generic compatibility does not certify this native P7 wire");
        assert!(pms_port(PmsCase::VerifiedDovi).qualify_pms(&off, verified).is_ok(),
            "a source-specific native P7 certificate can qualify an unmodified PMS copy");
        let mut off = pms_request(PmsRecipe::Remux);
        off.source_dovi = p7;
        off.output_dovi = Some(p7);
        off.dolby_vision_mode = plx_plex::plex::session::DolbyVisionMode::Off;
        assert_eq!(pms_port(PmsCase::VerifiedDovi).qualify_pms(&OffFrame::for_test(), off).err(),
            Some(PmsDecline::UnsupportedIntent),
            "DV Off cannot pass a raw copied Dolby Vision output");
        let mut missing_source_record = pms_request(PmsRecipe::Remux);
        missing_source_record.output_dovi = Some(p7);
        missing_source_record.dolby_vision_mode = plx_plex::plex::session::DolbyVisionMode::Off;
        assert_eq!(pms_port(PmsCase::VerifiedDovi).qualify_pms(&OffFrame::for_test(),
            missing_source_record).err(), Some(PmsDecline::UnsupportedIntent),
            "DV Off constrains the accepted output even if source metadata omitted DV");
    }

    #[test]
    fn explicitly_silent_pms_output_is_not_missing_audio_evidence() {
        let off = OffFrame::for_test();
        let mut request = pms_request(PmsRecipe::Encode);
        request.selection.audio_sid = 0;
        request.audio_output = PmsAudioOutput::Silent;
        let source = pms_port(PmsCase::Valid).qualify_pms(&off, request)
            .expect("a source with no audio and an accepted video-only output is silent");
        assert_eq!(source.requirements.audio_track, TrackChoice::Unspecified);
        let mut selected = pms_request(PmsRecipe::Encode);
        selected.audio_output = PmsAudioOutput::Silent;
        assert_eq!(pms_port(PmsCase::Valid).qualify_pms(&off, selected).err(),
            Some(PmsDecline::UnexecutableSelection));
    }

    struct Certified { dovi: Option<plx_data::metadata::Dovi>,
        subtitle: Option<SourceSubtitle>, expired: bool, mapped: bool }
    impl OriginalQualifier for Certified {
        fn qualify(&self, _off: &OffFrame, request: OriginalRequest) -> Verdict {
            let tracks = if self.mapped {
                let mut tracks = vec![NativeTrackIndex {
                    selection: TrackSelection { kind: TrackKind::Audio, id: 42 }, index: 1,
                }];
                if let Some(id) = u64::try_from(request.selection.subtitle_sid).ok().filter(|id| *id > 0) {
                    tracks.push(NativeTrackIndex {
                        selection: TrackSelection { kind: TrackKind::Subtitle, id }, index: 2,
                    });
                }
                tracks
            } else { Vec::new() };
            Verdict::Known(KnownSource { selection: request.selection,
                source: UriSource::new(request.source_url, tracks),
                requirements: Requirements { source: SourceKind::ProgressiveUri,
                    video: Fidelity::Exact, audio: Fidelity::Exact,
                    audio_track: TrackChoice::Selected(42),
                    subtitle_track: if request.selection.subtitle_sid > 0 {
                        TrackChoice::Selected(request.selection.subtitle_sid as u64)
                    } else { TrackChoice::Unspecified } },
                certified_dolby_vision: self.dovi,
                certified_subtitle_source: self.subtitle.clone(),
                expires_at: if self.expired { Instant::now() - std::time::Duration::from_secs(1) }
                    else { Instant::now() + std::time::Duration::from_secs(5) } })
        }
    }

    // ---- the viewer's opt-in (Settings > Playback > Playback engine) ----

    fn opt_in_request(uri_opt_in: bool) -> OriginalRequest {
        OriginalRequest { selection: SourceSelection {
                server: plx_plex::plex::ServerId::from_raw(1), part_id: 4, audio_sid: 42,
                subtitle_sid: 0, audio_contract: plx_plex::plex::AudioEnhancements::NONE },
            server_origin: plx_plex::plex::Origin::http("192.0.2.5", 32499),
            source_url: "https://example.invalid/part.mkv".to_owned(), duration_ms: Some(60_000),
            video_codec: "h264".to_owned(), dolby_vision: plx_data::metadata::Dovi::NONE,
            dolby_vision_mode: plx_plex::plex::session::DolbyVisionMode::Auto, uri_opt_in,
            standard: PLAYS, audio_codec: "aac".to_owned(), audio_ordinal: Some(1), has_multiple_audio_tracks: false, binding: Binding::Unbound,
            subtitle_source: None, subtitle_choice: TrackChoice::Unspecified,
            source_kbps: Some(4000), raster: Some((1280, 720)),
            requested_rate_permille: plx_playback_contract::RATE_NORMAL, resume_ns: 0,
            quality_allows_original: true, link_allows_original: true, auto_allows_original: true,
            requires_subtitle_burn: false, requires_client_subtitle: false, preview: false }
    }

    /// Standard direct-plays the request: the plan's four verdicts all say yes.
    const PLAYS: StandardDirectPlay = StandardDirectPlay { video: true, dolby_vision: true, audio: true,
        container: true };

    /// The production composition on an ACB set: nothing certified, `ProductQualifier`.
    fn production_port(binding: Binding) -> ResolvePort {
        ResolvePort::new(ProductEvidence::unqualified_on(plx_platform::tv::sink::VP_ACB, binding),
            Arc::new(ProductQualifier))
    }

    #[test]
    fn only_a_viewer_who_opted_in_gets_a_url_source_from_production() {
        let off = OffFrame::for_test();
        let port = production_port(Binding::Acb);
        assert!(matches!(port.qualify(&off, opt_in_request(false)), Verdict::Unknown(UriDecline::NotOptedIn)),
            "off keeps today's answer: nothing is qualified, so the route stays on Feed");
        let Verdict::Known(known) = port.qualify(&off, opt_in_request(true)) else {
            panic!("the opt-in is the viewer's qualification of their own URI source");
        };
        assert_eq!(known.requirements.source, SourceKind::ProgressiveUri);
        assert_eq!((known.requirements.video, known.requirements.audio), (Fidelity::Exact, Fidelity::Exact));
        assert_eq!(known.requirements.audio_track, TrackChoice::Selected(42));
        assert!(known.source.can_supply(known.requirements),
            "the selected audio stream maps to its native index");
        assert!(known.expires_at > Instant::now());
        // The port reports what everyone else gets: nothing certified.
        let evidence = port.evidence();
        assert!(!evidence.certified.progressive_uri && !evidence.certified.pause
            && !evidence.certified.seek && !evidence.certified.audio_track);
        assert!(!port.could_qualify_pms(), "a converted PMS output has no opt-in");
    }

    #[test]
    fn exported_uri_leaves_multi_audio_originals_on_standard() {
        let off = OffFrame::for_test();
        let port = ResolvePort::new(ProductEvidence::unqualified_on(
            plx_platform::tv::sink::VP_EXPORTED, Binding::Exported { canvas: (1920, 1080) }),
            Arc::new(ProductQualifier));
        let request = |ordinal, multiple| {
            let mut request = opt_in_request(true);
            request.audio_ordinal = Some(ordinal);
            request.has_multiple_audio_tracks = multiple;
            request
        };
        assert!(matches!(port.qualify(&off, request(0, false)), Verdict::Known(_)));
        for ordinal in [0, 1] {
            assert!(matches!(port.qualify(&off, request(ordinal, true)),
                Verdict::Refused(UriDecline::ManyAudioTracksExported)));
        }
        let acb = production_port(Binding::Acb);
        assert!(matches!(acb.qualify(&off, request(1, true)), Verdict::Known(_)));
    }

    #[test]
    fn the_opt_in_carries_only_an_original_standard_would_direct_play() {
        let off = OffFrame::for_test();
        let port = production_port(Binding::Acb);
        assert!(matches!(port.qualify(&off, opt_in_request(true)), Verdict::Known(_)), "the control");
        for (what, standard) in [
            ("a video the caps do not direct-play",
                StandardDirectPlay { video: false, ..PLAYS }),
            ("a Dolby Vision layering the frozen decision refuses",
                StandardDirectPlay { dolby_vision: false, ..PLAYS }),
            ("an audio track the set's own decoder does not play",
                StandardDirectPlay { audio: false, ..PLAYS }),
            ("a container Standard has the server remux",
                StandardDirectPlay { container: false, ..PLAYS }),
        ] {
            let mut request = opt_in_request(true);
            request.standard = standard;
            assert!(matches!(port.qualify(&off, request), Verdict::Unknown(_)),
                "{what} stays in Standard, whatever the opt-in");
        }
    }

    #[test]
    fn a_url_carrier_certifies_only_the_dolby_vision_the_frozen_decision_admitted() {
        let dovi = plx_data::metadata::Dovi { present: true, profile: 8, bl_compat: 1,
            ..plx_data::metadata::Dovi::NONE };
        let carry = |standard: StandardDirectPlay| {
            let mut request = opt_in_request(true);
            request.dolby_vision = dovi;
            request.standard = standard;
            KnownSource::url_carrier(&request, "https://example.invalid/part.mkv".to_owned(),
                SOURCE_VERDICT_TTL).certified_dolby_vision
        };
        assert_eq!(carry(PLAYS), Some(dovi), "an admitted layering is certified as the record itself");
        assert_eq!(carry(StandardDirectPlay { dolby_vision: false, ..PLAYS }), None,
            "a refused one is not, so ResolvePort::qualify's Dolby Vision check declines it");
        let mut plain = opt_in_request(true);
        plain.standard = StandardDirectPlay { dolby_vision: false, ..PLAYS };
        assert_eq!(KnownSource::url_carrier(&plain, "https://example.invalid/part.mkv".to_owned(),
            SOURCE_VERDICT_TTL).certified_dolby_vision, None, "a file with no Dolby Vision certifies none");
    }

    /// A qualifier that certifies Dolby Vision it was never admitted (here the carrier, handed a
    /// refusal) is still stopped by the exact-record check, so no path around the route's refusal.
    #[test]
    fn a_refused_dolby_vision_record_never_reaches_a_known_url_source() {
        let off = OffFrame::for_test();
        let dovi = plx_data::metadata::Dovi { present: true, profile: 5, bl_compat: 0,
            ..plx_data::metadata::Dovi::NONE };
        let mut request = opt_in_request(true);
        request.video_codec = "hevc".to_owned();
        request.dolby_vision = dovi;
        request.standard = StandardDirectPlay { video: false, dolby_vision: false, audio: true,
            container: true };
        let capabilities = Capabilities { progressive_uri: true, hls_uri: false, packet_feed: false,
            pause: true, seek: true, audio_track: true, subtitle_track: false,
            audio_off: false, subtitle_off: false };
        struct Carrier;
        impl OriginalQualifier for Carrier {
            fn qualify(&self, _off: &OffFrame, request: OriginalRequest) -> Verdict {
                let url = request.source_url.clone();
                Verdict::Known(KnownSource::url_carrier(&request, url, SOURCE_VERDICT_TTL))
            }
        }
        let port = ResolvePort::new(ProductEvidence { display_exported: false, binding: Binding::Acb,
            callable: capabilities, certified: capabilities, preview_rectangle: false },
            Arc::new(Carrier));
        assert!(matches!(port.qualify(&off, request), Verdict::Unknown(UriDecline::CertificateMismatch)));
    }

    #[test]
    fn the_opt_in_still_meets_every_viewer_refusal_and_the_display_path() {
        let off = OffFrame::for_test();
        let port = production_port(Binding::Acb);
        let mut preview = opt_in_request(true);
        preview.preview = true;
        assert!(matches!(port.qualify(&off, preview), Verdict::Refused(UriDecline::Preview)), "no preview rectangle");
        let mut relay = opt_in_request(true);
        relay.link_allows_original = false;
        assert!(matches!(port.qualify(&off, relay), Verdict::Refused(UriDecline::Relay)));
        let mut burn = opt_in_request(true);
        burn.requires_subtitle_burn = true;
        assert!(matches!(port.qualify(&off, burn), Verdict::Refused(UriDecline::SubtitleBurn)));
        let mut fast = opt_in_request(true);
        fast.requested_rate_permille = plx_playback_contract::RATE_NORMAL + 250;
        assert!(matches!(port.qualify(&off, fast), Verdict::Refused(UriDecline::Rate)));
        let mut subtitle = opt_in_request(true);
        subtitle.selection.subtitle_sid = 77;
        subtitle.subtitle_choice = TrackChoice::Selected(77);
        subtitle.subtitle_source = Some(SourceSubtitle { codec: "srt".to_owned(), embedded: true });
        assert!(matches!(port.qualify(&off, subtitle), Verdict::Unknown(UriDecline::SubtitleNotCarried)),
            "a selected embedded subtitle's render is not certified by an opt-in");
        let mut unmapped = opt_in_request(true);
        unmapped.audio_ordinal = None;
        assert!(matches!(port.qualify(&off, unmapped), Verdict::Unknown(UriDecline::CertificateMismatch)),
            "a selected audio stream without its native index cannot be carried");
        assert!(matches!(production_port(Binding::Unbound).qualify(&off, opt_in_request(true)),
            Verdict::Unknown(UriDecline::NoDisplayEvidence)), "no display path, no URI source");
    }

    #[test]
    fn dolby_vision_needs_an_exact_source_certificate() {
        let dovi = plx_data::metadata::Dovi { present: true, profile: 7, el_present: true,
            ..plx_data::metadata::Dovi::NONE };
        let capabilities = Capabilities { progressive_uri: true, hls_uri: false, packet_feed: false,
            pause: true, seek: true, audio_track: true, subtitle_track: true,
            audio_off: false, subtitle_off: false };
        let evidence = ProductEvidence { display_exported: true,
            binding: Binding::Exported { canvas: (1920, 1080) },
            callable: capabilities, certified: capabilities, preview_rectangle: false };
        let request = || OriginalRequest { selection: SourceSelection {
                server: plx_plex::plex::ServerId::from_raw(1), part_id: 4, audio_sid: 42,
                subtitle_sid: 0, audio_contract: plx_plex::plex::AudioEnhancements::NONE },
            server_origin: plx_plex::plex::Origin::http("192.0.2.5", 32499),
            source_url: "https://example.invalid/part.mkv".to_owned(), duration_ms: Some(60_000),
            video_codec: "hevc".to_owned(), dolby_vision: dovi,
            dolby_vision_mode: plx_plex::plex::session::DolbyVisionMode::Auto, uri_opt_in: false,
            standard: PLAYS, audio_codec: "ac3".to_owned(), audio_ordinal: Some(0), has_multiple_audio_tracks: false, binding: Binding::Unbound,
            subtitle_source: None,
            subtitle_choice: TrackChoice::Unspecified,
            source_kbps: Some(4000), raster: Some((1920, 1080)),
            requested_rate_permille: plx_playback_contract::RATE_NORMAL, resume_ns: 0,
            quality_allows_original: true, link_allows_original: true, auto_allows_original: true,
            requires_subtitle_burn: false, requires_client_subtitle: false, preview: false };
        let off = OffFrame::for_test();
        let unknown = ResolvePort::new(evidence, Arc::new(Certified { dovi: None,
            subtitle: None, expired: false, mapped: true }));
        assert!(matches!(unknown.qualify(&off, request()), Verdict::Unknown(UriDecline::CertificateMismatch)));
        let certified = ResolvePort::new(evidence, Arc::new(Certified { dovi: Some(dovi),
            subtitle: None, expired: false, mapped: true }));
        assert!(matches!(certified.qualify(&off, request()), Verdict::Known(_)));
        let unmapped = ResolvePort::new(evidence, Arc::new(Certified { dovi: Some(dovi),
            subtitle: None, expired: false, mapped: false }));
        assert!(matches!(unmapped.qualify(&off, request()), Verdict::Unknown(UriDecline::CertificateMismatch)),
            "a claimed selected audio track without its native index cannot reach the start door");
        let mut off_mode = request();
        off_mode.dolby_vision_mode = plx_plex::plex::session::DolbyVisionMode::Off;
        assert!(matches!(certified.qualify(&off, off_mode), Verdict::Refused(UriDecline::DolbyVisionOff)),
            "a raw URI may not bypass the viewer's disabled-DV presentation");
        let expired = ResolvePort::new(evidence, Arc::new(Certified { dovi: Some(dovi),
            subtitle: None, expired: true, mapped: true }));
        assert!(matches!(expired.qualify(&off, request()), Verdict::Unknown(UriDecline::CertificateMismatch)));

        let source_subtitle = SourceSubtitle { codec: "pgs".to_owned(), embedded: true };
        let subtitle_request = || { let mut input = request(); input.dolby_vision = plx_data::metadata::Dovi::NONE;
            input.selection.subtitle_sid = 77;
            input.subtitle_choice = TrackChoice::Selected(77);
            input.subtitle_source = Some(source_subtitle.clone()); input };
        let no_render_proof = ResolvePort::new(evidence, Arc::new(Certified { dovi: None,
            subtitle: None, expired: false, mapped: true }));
        assert!(matches!(no_render_proof.qualify(&off, subtitle_request()),
            Verdict::Unknown(UriDecline::SubtitleNotCarried)));
        let wrong_codec = ResolvePort::new(evidence, Arc::new(Certified { dovi: None,
            subtitle: Some(SourceSubtitle { codec: "vobsub".to_owned(), embedded: true }),
            expired: false, mapped: true }));
        assert!(matches!(wrong_codec.qualify(&off, subtitle_request()),
            Verdict::Unknown(UriDecline::SubtitleNotCarried)));
        let render_proof = ResolvePort::new(evidence, Arc::new(Certified { dovi: None,
            subtitle: Some(source_subtitle.clone()), expired: false, mapped: true }));
        assert!(matches!(render_proof.qualify(&off, subtitle_request()), Verdict::Known(_)));
        let mut off_request = request();
        off_request.dolby_vision = plx_data::metadata::Dovi::NONE;
        off_request.subtitle_choice = TrackChoice::Off;
        assert!(matches!(render_proof.qualify(&off, off_request), Verdict::Refused(UriDecline::SubtitleOff)),
            "the native driver cannot disable an otherwise selectable subtitle");
    }

    // ---- why the Experimental engine did not take a play (`UriDecline`) ----

    /// The kind of an answer and its reason, for a table: a reason must never change a kind.
    fn answer(verdict: &Verdict) -> (&'static str, Option<UriDecline>) {
        match verdict {
            Verdict::Known(_) => ("known", None),
            Verdict::Unknown(why) => ("unknown", Some(*why)),
            Verdict::Refused(why) => ("refused", Some(*why)),
        }
    }

    /// Every refusal and every unknown of [`ResolvePort::qualify`], one request each through the
    /// production composition, with the reason it names and the kind it always had.
    #[test]
    fn every_check_of_the_resolve_port_names_its_reason_and_keeps_its_kind() {
        let off = OffFrame::for_test();
        let port = production_port(Binding::Acb);
        let p8 = plx_data::metadata::Dovi { present: true, profile: 8, bl_compat: 1,
            ..plx_data::metadata::Dovi::NONE };
        type Tweak = fn(&mut OriginalRequest);
        let cases: [(&str, &str, Option<UriDecline>, Tweak); 17] = [
            ("the control", "known", None, |_| {}),
            ("speed", "refused", Some(UriDecline::Rate),
                |r| r.requested_rate_permille = plx_playback_contract::RATE_NORMAL + 250),
            ("quality ceiling", "refused", Some(UriDecline::QualityOrDirectPlay),
                |r| r.quality_allows_original = false),
            ("relay", "refused", Some(UriDecline::Relay), |r| r.link_allows_original = false),
            ("auto on a non-local link", "refused", Some(UriDecline::AutoOnNonLocalLink),
                |r| r.auto_allows_original = false),
            ("audio enhancement", "refused", Some(UriDecline::AudioEnhancement),
                |r| r.selection.audio_contract.boost_dialog = true),
            ("burned subtitle", "refused", Some(UriDecline::SubtitleBurn),
                |r| r.requires_subtitle_burn = true),
            ("external subtitle", "refused", Some(UriDecline::ClientSubtitle),
                |r| r.requires_client_subtitle = true),
            ("no subtitle selected", "refused", Some(UriDecline::SubtitleOff),
                |r| r.subtitle_choice = TrackChoice::Off),
            ("dolby vision off", "refused", Some(UriDecline::DolbyVisionOff), |r| {
                r.dolby_vision = plx_data::metadata::Dovi { present: true, profile: 8, bl_compat: 1,
                    ..plx_data::metadata::Dovi::NONE };
                r.dolby_vision_mode = plx_plex::plex::session::DolbyVisionMode::Off;
            }),
            ("preview", "refused", Some(UriDecline::Preview), |r| r.preview = true),
            ("no duration", "unknown", Some(UriDecline::NoDurationOrUrl), |r| r.duration_ms = None),
            ("no url", "unknown", Some(UriDecline::NoDurationOrUrl), |r| r.source_url.clear()),
            ("not opted in", "unknown", Some(UriDecline::NotOptedIn), |r| r.uri_opt_in = false),
            ("standard would convert the video", "unknown", Some(UriDecline::StandardVideo),
                |r| r.standard.video = false),
            // The product carrier certifies no subtitle render, so a selected embedded one ends here.
            ("a selected embedded subtitle", "unknown", Some(UriDecline::SubtitleNotCarried), |r| {
                r.selection.subtitle_sid = 77;
                r.subtitle_choice = TrackChoice::Selected(77);
                r.subtitle_source = Some(SourceSubtitle { codec: "srt".to_owned(), embedded: true });
            }),
            // The first check that fails names the reason, in the order the code tests them.
            ("quality and auto together", "refused", Some(UriDecline::QualityOrDirectPlay), |r| {
                r.quality_allows_original = false;
                r.auto_allows_original = false;
            }),
        ];
        for (what, kind, reason, tweak) in cases {
            let mut request = opt_in_request(true);
            tweak(&mut request);
            assert_eq!(answer(&port.qualify(&off, request)), (kind, reason), "{what}");
        }
        // A Dolby Vision file with the setting on Auto still carries (the record is certified).
        let mut dv = opt_in_request(true);
        dv.dolby_vision = p8;
        assert_eq!(answer(&port.qualify(&off, dv)).0, "known", "Dolby Vision on Auto");
    }

    /// The checks after a qualifier answered Known: each leaves the answer Unknown, and the one that
    /// is about the display path (not the certificate) says so.
    #[test]
    fn a_certificate_the_port_rejects_names_the_certificate_or_the_display_path() {
        let off = OffFrame::for_test();
        let capabilities = Capabilities { progressive_uri: true, hls_uri: false, packet_feed: false,
            pause: true, seek: true, audio_track: true, subtitle_track: true,
            audio_off: false, subtitle_off: false };
        let evidence = |binding| ProductEvidence { display_exported: true, binding,
            callable: capabilities, certified: capabilities, preview_rectangle: false };
        let exported = Binding::Exported { canvas: (1920, 1080) };
        let certify = |binding, expired, mapped| ResolvePort::new(evidence(binding),
            Arc::new(Certified { dovi: None, subtitle: None, expired, mapped }));
        assert_eq!(answer(&certify(exported, false, true).qualify(&off, opt_in_request(true))).0, "known");
        assert_eq!(answer(&certify(exported, true, true).qualify(&off, opt_in_request(true))),
            ("unknown", Some(UriDecline::CertificateMismatch)), "an expired certificate");
        assert_eq!(answer(&certify(exported, false, false).qualify(&off, opt_in_request(true))),
            ("unknown", Some(UriDecline::CertificateMismatch)), "a track the source cannot supply");
        // A certificate that matches the play on a path nothing can show is the display's gap, not
        // the certificate's: the exported window without a canvas, and an unbound executor.
        assert_eq!(answer(&certify(Binding::Exported { canvas: (0, 1080) }, false, true)
            .qualify(&off, opt_in_request(true))), ("unknown", Some(UriDecline::NoDisplayEvidence)));
        assert_eq!(answer(&certify(Binding::Unbound, false, true).qualify(&off, opt_in_request(true))),
            ("unknown", Some(UriDecline::NoDisplayEvidence)));
        // Both wrong: the certificate is judged first.
        assert_eq!(answer(&certify(Binding::Unbound, true, true).qualify(&off, opt_in_request(true))),
            ("unknown", Some(UriDecline::CertificateMismatch)));
        // A selected embedded subtitle the certificate does not carry has its own reason: after the
        // rest of the certificate, before the display path.
        let subtitled = || {
            let mut request = opt_in_request(true);
            request.selection.subtitle_sid = 77;
            request.subtitle_choice = TrackChoice::Selected(77);
            request.subtitle_source = Some(SourceSubtitle { codec: "srt".to_owned(), embedded: true });
            request
        };
        assert_eq!(answer(&certify(exported, false, true).qualify(&off, subtitled())),
            ("unknown", Some(UriDecline::SubtitleNotCarried)));
        assert_eq!(answer(&certify(exported, true, true).qualify(&off, subtitled())),
            ("unknown", Some(UriDecline::CertificateMismatch)), "an expired certificate first");
        assert_eq!(answer(&certify(Binding::Unbound, false, true).qualify(&off, subtitled())),
            ("unknown", Some(UriDecline::SubtitleNotCarried)), "the subtitle before the display path");
    }

    /// A qualifier's own Unknown or Refused reaches the caller with its reason, untouched.
    #[test]
    fn a_qualifiers_own_reason_passes_through_the_port() {
        struct Says(fn() -> Verdict);
        impl OriginalQualifier for Says {
            fn qualify(&self, _: &OffFrame, _: OriginalRequest) -> Verdict { (self.0)() }
        }
        let off = OffFrame::for_test();
        let through = |say: fn() -> Verdict| {
            let port = ResolvePort::new(ProductEvidence::unqualified_on(
                plx_platform::tv::sink::VP_ACB, Binding::Acb).opted_in(), Arc::new(Says(say)));
            answer(&port.qualify(&off, opt_in_request(true)))
        };
        assert_eq!(through(|| Verdict::Unknown(UriDecline::NotCertified)),
            ("unknown", Some(UriDecline::NotCertified)));
        assert_eq!(through(|| Verdict::Refused(UriDecline::Relay)),
            ("refused", Some(UriDecline::Relay)));
    }

    /// [`ProductQualifier`]'s own answers, called directly (the port fills the binding in before it
    /// asks, so a direct call names it).
    #[test]
    fn the_product_qualifier_names_why_it_did_not_certify() {
        let off = OffFrame::for_test();
        let exported = Binding::Exported { canvas: (1920, 1080) };
        let ask = |tweak: &dyn Fn(&mut OriginalRequest)| {
            let mut request = opt_in_request(true);
            request.binding = exported;
            tweak(&mut request);
            answer(&ProductQualifier.qualify(&off, request))
        };
        assert_eq!(ask(&|_| {}).0, "known");
        assert_eq!(ask(&|r| r.uri_opt_in = false), ("unknown", Some(UriDecline::NotOptedIn)));
        assert_eq!(ask(&|r| r.standard.video = false), ("unknown", Some(UriDecline::StandardVideo)));
        assert_eq!(ask(&|r| r.standard.dolby_vision = false),
            ("unknown", Some(UriDecline::StandardDolbyVision)));
        assert_eq!(ask(&|r| r.standard.audio = false), ("unknown", Some(UriDecline::StandardAudio)));
        assert_eq!(ask(&|r| r.standard.container = false),
            ("unknown", Some(UriDecline::StandardContainer)));
        assert_eq!(ask(&|r| r.has_multiple_audio_tracks = true),
            ("refused", Some(UriDecline::ManyAudioTracksExported)));
        assert_eq!(ask(&|r| { r.has_multiple_audio_tracks = true; r.binding = Binding::Acb }).0, "known",
            "several tracks are only a problem on the exported window");
        // Not opted in is named before what Standard would do, and the first gap before the rest.
        assert_eq!(ask(&|r| { r.uri_opt_in = false; r.standard.video = false }),
            ("unknown", Some(UriDecline::NotOptedIn)));
        assert_eq!(ask(&|r| { r.standard.audio = false; r.standard.container = false }),
            ("unknown", Some(UriDecline::StandardAudio)));
    }

    #[test]
    fn a_standard_gap_names_the_first_verdict_that_says_no() {
        assert_eq!(PLAYS.first_gap(), None);
        assert!(PLAYS.plays());
        let all_no = StandardDirectPlay { video: false, dolby_vision: false, audio: false, container: false };
        assert_eq!(all_no.first_gap(), Some(UriDecline::StandardVideo));
        assert!(!all_no.plays());
        assert_eq!(StandardDirectPlay { video: true, ..all_no }.first_gap(),
            Some(UriDecline::StandardDolbyVision));
        assert_eq!(StandardDirectPlay { video: true, dolby_vision: true, ..all_no }.first_gap(),
            Some(UriDecline::StandardAudio));
        assert_eq!(StandardDirectPlay { container: false, ..PLAYS }.first_gap(),
            Some(UriDecline::StandardContainer));
    }

    /// The log phrases are distinct, one line, and carry no parenthesis of their own: the line wraps
    /// the phrase in one.
    #[test]
    fn every_decline_has_its_own_one_line_phrase() {
        use UriDecline::*;
        let all = [NoPort, NotOffered, TrackPickUnresolved, Rate, QualityOrDirectPlay, Relay,
            AutoOnNonLocalLink, AudioEnhancement, SubtitleBurn, ClientSubtitle, SubtitleOff,
            DolbyVisionOff, Preview, NoDurationOrUrl, NotCertified, NotOptedIn, StandardVideo,
            StandardDolbyVision, StandardAudio, StandardContainer, ManyAudioTracksExported,
            CertificateMismatch, SubtitleNotCarried, NoDisplayEvidence];
        let mut seen = std::collections::HashSet::new();
        for why in all {
            let name = why.log_name();
            assert!(!name.is_empty() && !name.contains(['(', ')', '\n']), "{why:?}: {name:?}");
            assert!(seen.insert(name), "two reasons say {name:?}");
        }
        // A tester-facing phrase says what to change where there is something to change.
        assert!(AutoOnNonLocalLink.log_name().contains("set quality to Original"));
        assert!(SubtitleOff.log_name().contains("subtitle streams and none is selected"));
        assert!(SubtitleNotCarried.log_name().contains("try a file without subtitle streams"));
        assert!(QualityOrDirectPlay.log_name().contains("Direct Play to Auto"));
        assert!(ManyAudioTracksExported.log_name().contains("several audio tracks"));
    }
}
