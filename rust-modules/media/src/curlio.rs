//! Plex-facing authorization adapter for the shared HTTPS range transport.
//! The transport lives in `plx_net`; this alias binds its per-request URL check to the
//! live Plex credential grant, including reopens and redirect hops.

pub use plx_net::range_io::{
    abort_active, available, boot, media_stall_budget, Abort, BodyFailure, OpenErr,
    OpenReservation, ThroughputFailure, ThroughputSample, READ_DEADLINE, READ_STOPPED,
};

/// The media URL authority, read for each request rather than snapshotted at source creation.
pub struct PlexAuthorization;

impl plx_net::range_io::UrlAuthorizer for PlexAuthorization {
    fn allows(url: &str) -> bool {
        let (origin, path) = plx_net::net::origin::split(url);
        plx_plex::http::credential_transport_allowed(&origin, path, &[])
    }
}

pub type CurlSource = plx_net::range_io::CurlSource<PlexAuthorization>;

pub fn sample_throughput_result(
    url: &str, target_bytes: usize, setup_budget: std::time::Duration,
    body_budget: std::time::Duration,
) -> Result<ThroughputSample, ThroughputFailure> {
    plx_net::range_io::sample_throughput_result::<PlexAuthorization>(
        url, target_bytes, setup_budget, body_budget,
    )
}

pub fn sample_active_throughput_result<F>(
    url: &str, target_bytes: usize, setup_budget: std::time::Duration,
    body_budget: std::time::Duration, cancelled: F,
) -> Result<ThroughputSample, ThroughputFailure>
where
    F: FnOnce() -> bool,
{
    plx_net::range_io::sample_active_throughput_result::<PlexAuthorization, F>(
        url, target_bytes, setup_budget, body_budget, cancelled,
    )
}

#[cfg(test)]
fn media_url_allowed_by_policy(url: &str, policy: plx_plex::plex::CredentialPolicy) -> bool {
    let (origin, path) = plx_net::net::origin::split(url);
    plx_plex::http::credential_transport_allowed_by_policy(&origin, path, &[], policy)
}

/// What the `curlio_*_tests.rs` files share: one body, one fixture host name and the pin that
/// sends it to loopback.
#[cfg(test)]
mod test_fixtures {
    use plx_net::net::origin::ResolvePin;
    use plx_net::net::resolve;

    pub(super) fn media_body() -> Vec<u8> {
        (0..5000u32).map(|i| (i % 253) as u8).collect()
    }

    pub(super) const PLEX_DIRECT: &str = "127-0-0-1.0123456789abcdef0123456789abcdef.plex.direct";

    pub(super) fn pin_to_loopback(host: &str, port: u16) {
        resolve::add(&ResolvePin::for_test(host, i32::from(port), std::net::IpAddr::from([127, 0, 0, 1])));
    }
}

/// Issue #378's media-plane half: key mode through a real `CurlSource` handshake.
#[cfg(test)]
#[path = "curlio_keymode_tests.rs"]
mod keymode_tests;

/// The bundled public roots fallback, media plane: the same open/reopen/seek paths through the
/// shared decision in `net::keypin`.
#[cfg(test)]
#[path = "curlio_roots_tests.rs"]
mod roots_tests;

/// What a media open does with a `3xx` (hop cap, no downgrade, `Range`, relative `Location`, the
/// token, and the TLS decision each hop makes for its own host), one hop at a time.
#[cfg(test)]
#[path = "curlio_redirect_tests.rs"]
mod redirect_tests;


#[cfg(test)]
mod policy_tests {
    use super::*;
    #[test]
    fn lower_media_layer_refuses_plaintext_credentials_in_store_policy() {
        assert!(!media_url_allowed_by_policy(
            "http://192.0.2.1:32400/video.mkv?X-Plex-Token=secret",
            plx_plex::plex::CredentialPolicy::HttpsOnly,
        ));
        assert!(media_url_allowed_by_policy(
            "https://example.invalid/video.mkv?X-Plex-Token=secret",
            plx_plex::plex::CredentialPolicy::HttpsOnly,
        ));
        assert!(media_url_allowed_by_policy(
            "http://192.0.2.1:32400/video.mkv?X-Plex-Token=secret",
            plx_plex::plex::CredentialPolicy::AllowPlaintext,
        ));
    }

}
