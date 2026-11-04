//! Source-specific URI qualification on the existing off-frame resolve worker.
//! A native implementation may answer Unknown until its display and control behavior is proven.

use std::sync::Arc;
use std::time::Instant;

use plx_base::task::OffFrame;
use plx_playback_contract::uri::{Binding, NativeTrackIndex, UriSource};
use plx_playback_contract::{Capabilities, Fidelity, Requirements, SourceKind, TrackChoice, TrackKind};

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
    pub fn unqualified(display_mode: i32) -> Self {
        Self::unqualified_on(display_mode, Binding::Unbound)
    }

    /// [`unqualified`](Self::unqualified) with the display path composition chose for the
    /// executor (the URI driver's `product_binding`). The binding configures the executor only: `permits` and
    /// `could_qualify_pms` still require `Binding::Exported`, so no source is offered through it.
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

    pub fn permits(&self, requirements: Requirements, preview: bool) -> bool {
        self.display_exported
            && matches!(self.binding, Binding::Exported { canvas: (w, h) } if w > 0 && h > 0)
            && (!preview || self.preview_rectangle)
            && self.callable.pause && self.callable.seek
            && self.certified.pause && self.certified.seek
            && self.callable.supports(requirements)
            && self.certified.supports(requirements)
    }
}

/// URI source facts gathered from the selected PMS Part before Feed picks a sibling or encoder.
/// The Part URL and stream identifiers are intentionally absent from Debug/log output.
pub struct OriginalRequest {
    pub selection: SourceSelection,
    pub source_url: String,
    pub duration_ms: Option<u64>,
    pub video_codec: String,
    /// Exact selected-source layering; a positive DV verdict must certify this record.
    pub dolby_vision: plx_data::metadata::Dovi,
    pub dolby_vision_mode: plx_plex::plex::session::DolbyVisionMode,
    pub audio_codec: String,
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

pub enum Verdict { Known(KnownSource), Unknown, Refused }

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
/// from Feed's codec table, MDE answer or native ACK.
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
        self.evidence.display_exported
            && matches!(self.evidence.binding, Binding::Exported { canvas: (w, h) } if w > 0 && h > 0)
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

    pub fn qualify(&self, off: &OffFrame, request: OriginalRequest) -> Verdict {
        if request.requested_rate_permille != plx_playback_contract::RATE_NORMAL
            || !request.quality_allows_original || !request.link_allows_original
            || !request.auto_allows_original || request.selection.audio_contract.any()
            || request.requires_subtitle_burn || request.requires_client_subtitle
            || request.subtitle_choice == TrackChoice::Off
            || (request.dolby_vision.present
                && request.dolby_vision_mode == plx_plex::plex::session::DolbyVisionMode::Off)
            || (request.preview && !self.evidence.preview_rectangle) {
            return Verdict::Refused;
        }
        if request.duration_ms.is_none() || request.source_url.is_empty() {
            return Verdict::Unknown;
        }
        let preview = request.preview;
        let selected = request.selection;
        let dovi = request.dolby_vision;
        let subtitle = request.subtitle_source.clone();
        let subtitle_choice = request.subtitle_choice;
        match self.qualifier.qualify(off, request) {
            Verdict::Known(known) if known.selection == selected
                && known.requirements.source == SourceKind::ProgressiveUri
                && known.requirements.video == Fidelity::Exact
                && known.requirements.audio == Fidelity::Exact
                && known.source.can_supply(known.requirements)
                && (!dovi.present || known.certified_dolby_vision == Some(dovi))
                && known.requirements.subtitle_track == subtitle_choice
                && (selected.subtitle_sid <= 0 || (subtitle.as_ref().is_some_and(|sub| sub.embedded)
                    && known.certified_subtitle_source == subtitle))
                && (selected.audio_sid <= 0 || known.requirements.audio_track
                    == TrackChoice::Selected(selected.audio_sid as u64))
                && (selected.subtitle_sid <= 0 || known.requirements.subtitle_track
                    == TrackChoice::Selected(selected.subtitle_sid as u64))
                && known.expires_at > Instant::now()
                && self.evidence.permits(known.requirements, preview) =>
                Verdict::Known(known),
            Verdict::Known(_) => Verdict::Unknown,
            other => other,
        }
    }
}

/// Native product source evidence is presently unqualified. The same port is nevertheless run
/// by every real resolve and can consume a future certified provider without changing route policy.
pub struct ProductQualifier;
impl OriginalQualifier for ProductQualifier {
    fn qualify(&self, _off: &OffFrame, _request: OriginalRequest) -> Verdict { Verdict::Unknown }
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

    const MASTER: &[u8] = b"#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1000000\nlow/index.m3u8\n#EXT-X-STREAM-INF:BANDWIDTH=3000000\nhigh/index.m3u8\n";

    #[derive(Clone, Copy)]
    enum PmsCase { Valid, VerifiedDovi, Unknown, Refused, WrongSource, WrongSession,
        WrongOutputCodec, UnknownClock,
        NoRelease, Expired, FixedHls, FalseMaster }

    struct CertifiedPms(PmsCase);
    impl OriginalQualifier for CertifiedPms {
        fn qualify(&self, _: &OffFrame, _: OriginalRequest) -> Verdict { Verdict::Unknown }

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
            source_url: "https://example.invalid/part.mkv".to_owned(), duration_ms: Some(60_000),
            video_codec: "hevc".to_owned(), dolby_vision: dovi,
            dolby_vision_mode: plx_plex::plex::session::DolbyVisionMode::Auto,
            audio_codec: "ac3".to_owned(),
            subtitle_source: None,
            subtitle_choice: TrackChoice::Unspecified,
            source_kbps: Some(4000), raster: Some((1920, 1080)),
            requested_rate_permille: plx_playback_contract::RATE_NORMAL, resume_ns: 0,
            quality_allows_original: true, link_allows_original: true, auto_allows_original: true,
            requires_subtitle_burn: false, requires_client_subtitle: false, preview: false };
        let off = OffFrame::for_test();
        let unknown = ResolvePort::new(evidence, Arc::new(Certified { dovi: None,
            subtitle: None, expired: false, mapped: true }));
        assert!(matches!(unknown.qualify(&off, request()), Verdict::Unknown));
        let certified = ResolvePort::new(evidence, Arc::new(Certified { dovi: Some(dovi),
            subtitle: None, expired: false, mapped: true }));
        assert!(matches!(certified.qualify(&off, request()), Verdict::Known(_)));
        let unmapped = ResolvePort::new(evidence, Arc::new(Certified { dovi: Some(dovi),
            subtitle: None, expired: false, mapped: false }));
        assert!(matches!(unmapped.qualify(&off, request()), Verdict::Unknown),
            "a claimed selected audio track without its native index cannot reach the start door");
        let mut off_mode = request();
        off_mode.dolby_vision_mode = plx_plex::plex::session::DolbyVisionMode::Off;
        assert!(matches!(certified.qualify(&off, off_mode), Verdict::Refused),
            "a raw URI may not bypass the viewer's disabled-DV presentation");
        let expired = ResolvePort::new(evidence, Arc::new(Certified { dovi: Some(dovi),
            subtitle: None, expired: true, mapped: true }));
        assert!(matches!(expired.qualify(&off, request()), Verdict::Unknown));

        let source_subtitle = SourceSubtitle { codec: "pgs".to_owned(), embedded: true };
        let subtitle_request = || { let mut input = request(); input.dolby_vision = plx_data::metadata::Dovi::NONE;
            input.selection.subtitle_sid = 77;
            input.subtitle_choice = TrackChoice::Selected(77);
            input.subtitle_source = Some(source_subtitle.clone()); input };
        let no_render_proof = ResolvePort::new(evidence, Arc::new(Certified { dovi: None,
            subtitle: None, expired: false, mapped: true }));
        assert!(matches!(no_render_proof.qualify(&off, subtitle_request()), Verdict::Unknown));
        let wrong_codec = ResolvePort::new(evidence, Arc::new(Certified { dovi: None,
            subtitle: Some(SourceSubtitle { codec: "vobsub".to_owned(), embedded: true }),
            expired: false, mapped: true }));
        assert!(matches!(wrong_codec.qualify(&off, subtitle_request()), Verdict::Unknown));
        let render_proof = ResolvePort::new(evidence, Arc::new(Certified { dovi: None,
            subtitle: Some(source_subtitle.clone()), expired: false, mapped: true }));
        assert!(matches!(render_proof.qualify(&off, subtitle_request()), Verdict::Known(_)));
        let mut off_request = request();
        off_request.dolby_vision = plx_data::metadata::Dovi::NONE;
        off_request.subtitle_choice = TrackChoice::Off;
        assert!(matches!(render_proof.qualify(&off, off_request), Verdict::Refused),
            "the native driver cannot disable an otherwise selectable subtitle");
    }
}
