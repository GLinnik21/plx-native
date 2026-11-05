//! Owned, resolved facts for one physical buffer-feed open. Route selection and credential
//! authorization finish before this reaches the native constructor. URLs have no Debug surface.

pub struct PhysicalOpen {
    pub origin: plx_net::net::origin::Origin,
    pub path: String,
    pub video_codec: String,
    pub audio_codec: String,
    pub pcm_is_lossy: bool,
    pub audio_channels: i64,
    pub feed_payload: crate::PayloadFacts,
    pub payload_options: crate::PayloadOptions,
    pub original_dv: OriginalDvFacts,
    pub session: Option<Box<dyn plx_playback_contract::demux::DemuxSession>>,
    pub hls_abr: Option<Box<dyn plx_playback_contract::adaptive::AdaptiveSession>>,
    pub auto_original: Option<Box<dyn plx_playback_contract::adaptive::OriginalSession>>,
}

pub use plx_playback_contract::feed::OriginalDvFacts;
