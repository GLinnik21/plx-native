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

/// A per-job factory. The policy itself is read at each request by `PlexAuthorization`, so
/// capturing this value cannot freeze or bypass a later credential revocation.
pub fn range_factory() -> std::sync::Arc<dyn plx_net::range_io::RangeFactory> {
    std::sync::Arc::new(plx_net::range_io::AuthorizingRangeFactory::<PlexAuthorization>::new())
}

pub const CREDENTIAL_GATE: plx_net::stream::redirect::CredentialGate =
    plx_plex::http::credential_transport_allowed;

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

/// Why [`first_read`] came back without bytes; `Display` is the short reason the TLS self-test
/// prints (`open=…`, `rc=…`, `read=…`).
#[cfg(feature = "devtriggers")]
#[derive(Debug, PartialEq, Eq)]
pub enum FirstReadFailure {
    /// No transfer slot could be reserved.
    Reserve(OpenErr),
    /// The open failed: libcurl's own code for a transport failure, else the typed refusal.
    Open(OpenErr),
    /// The open succeeded and the read returned this non-positive count.
    Read(std::os::raw::c_int),
}

#[cfg(feature = "devtriggers")]
impl std::fmt::Display for FirstReadFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Open(OpenErr::Transport(rc)) => write!(f, "rc={rc}"),
            Self::Reserve(e) | Self::Open(e) => write!(f, "open={e:?}"),
            Self::Read(n) => write!(f, "read={n}"),
        }
    }
}

/// Open `url` at offset 0 through a real [`CurlSource`] and read its first bytes into `dst`, both
/// bounded by `until` (the TLS self-test's media-plane half). It is not generic on purpose: the
/// application calls this instead of naming `CurlSource<PlexAuthorization>`, so only this crate
/// instantiates that transport and the binary carries one copy of it.
#[cfg(feature = "devtriggers")]
pub fn first_read(url: &str, until: std::time::Instant, dst: &mut [u8]) -> Result<usize, FirstReadFailure> {
    let reservation = CurlSource::reserve_open().map_err(FirstReadFailure::Reserve)?;
    let mut src = CurlSource::open_reserved_checked(
        url, 0, reservation, Some(until), &mut plx_base::checkpoint::NoCheckpoint,
    ).map_err(FirstReadFailure::Open)?;
    let n = src.read_until(dst, Some(until), &mut plx_base::checkpoint::NoCheckpoint);
    if n <= 0 {
        return Err(FirstReadFailure::Read(n));
    }
    Ok(n as usize)
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

#[cfg(all(test, feature = "devtriggers"))]
mod first_read_tests {
    use super::*;
    use plx_net::net::{curl_ready, expired_leaf, remember, spawn_observed, TestCaGuard};
    use std::sync::Arc;

    fn until() -> std::time::Instant { std::time::Instant::now() + std::time::Duration::from_secs(10) }

    #[test]
    fn reads_the_first_bytes_of_a_served_body_through_the_media_transport() {
        let _serial = plx_base::testlock::serial();
        if !curl_ready() { return; }
        let cert = expired_leaf(&["127.0.0.1"]);
        let _ca = TestCaGuard::install(&cert.pem, "first-read-ok");
        let served = spawn_observed(Arc::clone(&cert), test_fixtures::media_body());
        let _key = remember(served.port, &cert);
        let mut buf = [0u8; 64];
        let n = first_read(&format!("https://127.0.0.1:{}/video.mkv", served.port), until(), &mut buf)
            .expect("the remembered key admits the server");
        assert!(n > 0 && n <= 64);
        assert_eq!(buf[..n], test_fixtures::media_body()[..n]);
    }

    #[test]
    fn a_closed_port_is_reported_as_libcurls_own_code_and_prints_as_rc() {
        let _serial = plx_base::testlock::serial();
        if !curl_ready() { return; }
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let err = first_read(&format!("https://127.0.0.1:{port}/video.mkv"), until(), &mut [0u8; 8])
            .expect_err("nothing listens");
        assert_eq!(err, FirstReadFailure::Open(OpenErr::Transport(7)));
        assert_eq!(err.to_string(), "rc=7");
    }

    #[test]
    fn the_failure_texts_are_the_self_tests_own() {
        assert_eq!(FirstReadFailure::Read(-1).to_string(), "read=-1");
        assert_eq!(FirstReadFailure::Read(0).to_string(), "read=0");
        assert_eq!(FirstReadFailure::Open(OpenErr::Transport(60)).to_string(), "rc=60");
        assert_eq!(FirstReadFailure::Reserve(OpenErr::Transport(60)).to_string(), format!("open={:?}", OpenErr::Transport(60)));
    }
}
