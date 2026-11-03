//! Source-specific URI qualification on the existing off-frame resolve worker.
//! A native implementation may answer Unknown until its display and control behavior is proven.

use std::sync::Arc;
use std::time::Instant;

use plx_base::task::OffFrame;
use plx_play_uri::backend::{Binding, UriSource};
use plx_playback_contract::{Capabilities, Fidelity, Requirements, SourceKind, TrackChoice};

use super::SourceSelection;

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
        let exported = display_mode == plx_platform::tv::sink::VP_EXPORTED;
        let callable = Capabilities { progressive_uri: true, hls_uri: true, packet_feed: false,
            pause: true, seek: true, audio_track: true, subtitle_track: true,
            audio_off: false, subtitle_off: false };
        let certified = Capabilities { progressive_uri: false, hls_uri: false, packet_feed: false,
            pause: false, seek: false, audio_track: false, subtitle_track: false,
            audio_off: false, subtitle_off: false };
        Self { display_exported: exported, binding: Binding::Unbound, callable,
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
    pub audio_codec: String,
    pub source_kbps: Option<u32>,
    pub raster: Option<(u32, u32)>,
    pub requested_rate_permille: i32,
    pub resume_ns: i64,
    pub quality_allows_original: bool,
    pub link_allows_original: bool,
    pub auto_allows_original: bool,
    pub requires_subtitle_burn: bool,
    pub preview: bool,
}

pub struct KnownSource {
    pub selection: SourceSelection,
    pub source: UriSource,
    pub requirements: Requirements,
    /// Source-specific DV certificate. `None` is enough only when PMS reports no DV.
    pub certified_dolby_vision: Option<plx_data::metadata::Dovi>,
    pub expires_at: Instant,
}

pub enum Verdict { Known(KnownSource), Unknown, Refused }

/// The caller supplies the actual Part URL and selected source tracks. An implementation may
/// perform bounded IO only with the worker's OffFrame proof; it must not infer a URI certificate
/// from Feed's codec table, MDE answer or native ACK.
pub trait OriginalQualifier: Send + Sync {
    fn qualify(&self, off: &OffFrame, request: OriginalRequest) -> Verdict;
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

    pub fn qualify(&self, off: &OffFrame, request: OriginalRequest) -> Verdict {
        if request.requested_rate_permille != plx_playback_contract::RATE_NORMAL
            || !request.quality_allows_original || !request.link_allows_original
            || !request.auto_allows_original || request.selection.audio_contract.any()
            || request.requires_subtitle_burn || (request.preview && !self.evidence.preview_rectangle) {
            return Verdict::Refused;
        }
        if request.duration_ms.is_none() || request.source_url.is_empty() {
            return Verdict::Unknown;
        }
        let preview = request.preview;
        let selected = request.selection;
        let dovi = request.dolby_vision;
        match self.qualifier.qualify(off, request) {
            Verdict::Known(known) if known.selection == selected
                && known.requirements.source == SourceKind::ProgressiveUri
                && known.requirements.video == Fidelity::Exact
                && known.requirements.audio == Fidelity::Exact
                && (!dovi.present || known.certified_dolby_vision == Some(dovi))
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

    struct Certified { dovi: Option<plx_data::metadata::Dovi>, expired: bool }
    impl OriginalQualifier for Certified {
        fn qualify(&self, _off: &OffFrame, request: OriginalRequest) -> Verdict {
            Verdict::Known(KnownSource { selection: request.selection,
                source: UriSource::new(request.source_url, Vec::new()),
                requirements: Requirements { source: SourceKind::ProgressiveUri,
                    video: Fidelity::Exact, audio: Fidelity::Exact,
                    audio_track: TrackChoice::Selected(42), subtitle_track: TrackChoice::Unspecified },
                certified_dolby_vision: self.dovi,
                expires_at: if self.expired { Instant::now() - std::time::Duration::from_secs(1) }
                    else { Instant::now() + std::time::Duration::from_secs(5) } })
        }
    }

    #[test]
    fn dolby_vision_needs_an_exact_source_certificate() {
        let dovi = plx_data::metadata::Dovi { present: true, profile: 7, el_present: true,
            ..plx_data::metadata::Dovi::NONE };
        let capabilities = Capabilities { progressive_uri: true, hls_uri: false, packet_feed: false,
            pause: true, seek: true, audio_track: true, subtitle_track: false,
            audio_off: false, subtitle_off: false };
        let evidence = ProductEvidence { display_exported: true,
            binding: Binding::Exported { canvas: (1920, 1080) },
            callable: capabilities, certified: capabilities, preview_rectangle: false };
        let request = || OriginalRequest { selection: SourceSelection {
                server: plx_plex::plex::ServerId::from_raw(1), part_id: 4, audio_sid: 42,
                subtitle_sid: 0, audio_contract: plx_plex::plex::AudioEnhancements::NONE },
            source_url: "https://example.invalid/part.mkv".to_owned(), duration_ms: Some(60_000),
            video_codec: "hevc".to_owned(), dolby_vision: dovi, audio_codec: "ac3".to_owned(),
            source_kbps: Some(4000), raster: Some((1920, 1080)),
            requested_rate_permille: plx_playback_contract::RATE_NORMAL, resume_ns: 0,
            quality_allows_original: true, link_allows_original: true, auto_allows_original: true,
            requires_subtitle_burn: false, preview: false };
        let off = OffFrame::for_test();
        let unknown = ResolvePort::new(evidence, Arc::new(Certified { dovi: None, expired: false }));
        assert!(matches!(unknown.qualify(&off, request()), Verdict::Unknown));
        let certified = ResolvePort::new(evidence, Arc::new(Certified { dovi: Some(dovi), expired: false }));
        assert!(matches!(certified.qualify(&off, request()), Verdict::Known(_)));
        let expired = ResolvePort::new(evidence, Arc::new(Certified { dovi: Some(dovi), expired: true }));
        assert!(matches!(expired.qualify(&off, request()), Verdict::Unknown));
    }
}
