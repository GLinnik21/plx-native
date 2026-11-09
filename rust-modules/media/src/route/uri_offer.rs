//! The finite URI offer the route owner prepares for a resolved source: one qualified recipe, its
//! owned source and the requirements it was certified against. It is route data: the player
//! adapter stages it into whichever URI driver the composition supplies, and no driver crate is
//! named here.

use plx_playback_contract::uri::UriSource;
use plx_playback_contract::Failure;

/// An external IO qualifier's verdict for one resolved Part and its exact selected streams.
/// The owned URL and native track map never enter the route machine or a Debug value. Production
/// supplies `OriginalPart` only for a viewer who opted in (`OriginalRequest::uri_opt_in`), only for
/// an original Standard would direct-play (`OriginalRequest::standard`: its video, Dolby Vision, audio
/// and container verdicts, not the server's own decision, which the offer is made before) and never a
/// converted PMS output; for everyone else absence preserves the Feed choice.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UriSourceRecipe {
    OriginalPart,
    PmsRemux { applied_audio: plx_plex::plex::AudioEnhancements },
    PmsEncode { applied_audio: plx_plex::plex::AudioEnhancements },
    PmsHls { applied_audio: plx_plex::plex::AudioEnhancements },
}

impl UriSourceRecipe {
    pub fn offer_id(self) -> super::backend_policy::OfferId {
        use super::backend_policy::OfferId;
        match self {
            Self::OriginalPart => OfferId::URI_ORIGINAL,
            Self::PmsRemux { .. } => OfferId::URI_PMS_REMUX,
            Self::PmsEncode { .. } => OfferId::URI_PMS_ENCODE,
            Self::PmsHls { .. } => OfferId::URI_PMS_HLS,
        }
    }

    pub fn audio(self) -> plx_plex::plex::AudioEnhancements {
        match self {
            Self::OriginalPart => plx_plex::plex::AudioEnhancements::NONE,
            Self::PmsRemux { applied_audio } | Self::PmsEncode { applied_audio }
            | Self::PmsHls { applied_audio } => applied_audio,
        }
    }
}

pub struct QualifiedUriOffer {
    pub offer_id: super::backend_policy::OfferId,
    pub recipe: UriSourceRecipe,
    pub route_start: super::RouteStartTransaction,
    pub selection: super::UriSelection,
    pub source: UriSource,
    pub requirements: plx_playback_contract::Requirements,
    pub initial: plx_playback_contract::InitialPlayback,
    /// The resolved item's PMS metadata duration, supplied with this source-specific offer.
    pub pms_duration_ns: i64,
    pub expires_at: std::time::Instant,
}

/// One physical URI failure awaiting an exact native release verdict before a Feed resolve.
/// This records evidence about the canonical attempt; it does not queue or admit a successor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UriRecoveryNotice {
    pub attempt: super::RouteStartAttempt,
    pub offer_id: super::backend_policy::OfferId,
    pub phase: super::UriFailurePhase,
    pub known_position_ns: Option<i64>,
    pub viewer_paused: bool,
}

impl QualifiedUriOffer {
    /// Stable recipe identity retained when an addressed offer cannot be installed.
    pub fn offer_id(&self) -> super::backend_policy::OfferId { self.offer_id }

    pub fn new(offer_id: super::backend_policy::OfferId,
        recipe: UriSourceRecipe,
        route_start: super::RouteStartTransaction,
        selection: super::UriSelection, source: UriSource,
        requirements: plx_playback_contract::Requirements,
        initial: plx_playback_contract::InitialPlayback,
        pms_duration_ms: u64,
        expires_at: std::time::Instant) -> Result<Self, Failure> {
        use super::backend_policy::OfferId;
        let required_fidelity = match offer_id {
            OfferId::URI_ORIGINAL => plx_playback_contract::Fidelity::Exact,
            OfferId::URI_PMS_REMUX | OfferId::URI_PMS_ENCODE | OfferId::URI_PMS_HLS =>
                plx_playback_contract::Fidelity::Compatible,
            _ => return Err(Failure::Unsupported),
        };
        if recipe.offer_id() != offer_id || recipe.audio() != selection.audio_contract
            || requirements.source == plx_playback_contract::SourceKind::PacketFeed
            || requirements.video != required_fidelity
            || requirements.audio != required_fidelity
            || (offer_id == OfferId::URI_PMS_HLS
                && requirements.source != plx_playback_contract::SourceKind::HlsUri)
            || (matches!(offer_id, OfferId::URI_PMS_REMUX | OfferId::URI_PMS_ENCODE)
                && requirements.source != plx_playback_contract::SourceKind::ProgressiveUri) {
            return Err(Failure::Unsupported);
        }
        let pms_duration_ns = pms_duration_ms.checked_mul(1_000_000)
            .and_then(|ns| i64::try_from(ns).ok()).filter(|ns| *ns > 0)
            .ok_or(Failure::Unsupported)?;
        if initial.position.is_some_and(|position| position.as_millis()
            .checked_mul(1_000_000)
            .and_then(|ns| i64::try_from(ns).ok())
            .is_none_or(|ns| ns > pms_duration_ns)) {
            return Err(Failure::Unsupported);
        }
        let selected_audio = (selection.audio_sid > 0).then(||
            plx_playback_contract::TrackChoice::Selected(selection.audio_sid as u64));
        let selected_subtitle = (selection.subtitle_sid > 0).then(||
            plx_playback_contract::TrackChoice::Selected(selection.subtitle_sid as u64));
        if (offer_id == OfferId::URI_ORIGINAL
                && (selected_audio.is_some_and(|choice| requirements.audio_track != choice)
                    || selected_subtitle.is_some_and(|choice| requirements.subtitle_track != choice)))
            || selected_audio.is_some_and(|choice| matches!(requirements.audio_track,
                plx_playback_contract::TrackChoice::Selected(_) if requirements.audio_track != choice))
            || selected_subtitle.is_some_and(|choice| matches!(requirements.subtitle_track,
                plx_playback_contract::TrackChoice::Selected(_) if requirements.subtitle_track != choice))
            || (selection.audio_sid <= 0 && matches!(requirements.audio_track,
                plx_playback_contract::TrackChoice::Selected(_)))
            || (selection.subtitle_sid <= 0 && matches!(requirements.subtitle_track,
                plx_playback_contract::TrackChoice::Selected(_))) {
            return Err(Failure::Unsupported);
        }
        Ok(Self { offer_id, recipe, route_start, selection, source, requirements, initial,
            pms_duration_ns, expires_at })
    }
}

