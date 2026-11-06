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

impl From<plx_playback_contract::feed::OpenStream> for PhysicalOpen {
    /// The feed's plain request becomes the executor's open; the demux session is bound later
    /// ([`crate::native_executor::NativePrepared::bind_demux_session`]).
    fn from(stream: plx_playback_contract::feed::OpenStream) -> Self {
        use plx_net::net::origin::{Origin, Scheme};
        let scheme = if stream.origin.tls { Scheme::Https } else { Scheme::Http };
        Self {
            origin: Origin::new(scheme, &stream.origin.host, stream.origin.port),
            path: stream.path,
            video_codec: stream.video_codec,
            audio_codec: stream.audio_codec,
            pcm_is_lossy: stream.pcm_is_lossy,
            audio_channels: stream.audio_channels,
            feed_payload: stream.feed_payload,
            payload_options: stream.payload_options,
            original_dv: stream.original_dv,
            session: None,
            hls_abr: stream.hls_abr,
            auto_original: stream.auto_original,
        }
    }
}
