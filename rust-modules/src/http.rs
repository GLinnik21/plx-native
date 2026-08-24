//! **The one door out of the control plane** — the single place that decides which transport a
//! Plex REST request takes, and the only place that knows there is more than one.
//!
//! ## Why this file exists
//!
//! The control plane has two request transports and they are not interchangeable:
//!
//! * [`crate::stream`] is a raw TCP socket. It resolves through `getaddrinfo` and dials either
//!   address family, it is fast, and it speaks **cleartext only**.
//! * [`crate::net`] is libcurl. It also validates certificates, and it is what plex.tv has always
//!   been reached through.
//!
//! Media makes its own scheme decision under `ff.rs`: the same `stream.rs` socket for plaintext,
//! or [`crate::curlio`]'s interruptible libcurl-multi pull source for HTTPS. That is deliberately
//! outside this REST request façade.
//!
//! **TLS is now the only thing that separates them**, which `stream.rs`'s own module doc says in
//! those words. Until this file, the PMS control plane was hard-wired to the first of the two, and
//! that is the whole of why a reviewer with no Plex Media Server on their LAN dead-ends: an account
//! signed in from anywhere else reaches its servers over `https://<something>.plex.direct` — a name
//! carrying a certificate, and cleartext to the address behind it fails validation by design
//! (`plex::origin`).
//!
//! So the dispatch is on [`Origin::scheme`] and nowhere else. An [`Origin`] is parsed from a URL
//! and never rebuilt from an address, so by the time a request reaches here the question "which
//! transport" has exactly one answer and no call site has to know it.
//!
//! ## What it returns, and why that is the interesting half
//!
//! [`Reply`] carries the **status and the body**, always.
//!
//! `stream.rs` used to offer three one-shot wrappers — `http_get`, `http_put`, `http_post` — and
//! each folded away half the answer: the first two returned `Option<Vec<u8>>`, collapsing every
//! non-2xx into the same `None` a refused connection produces, and the third returned the status
//! with the body dropped. That collapse is precisely the bug [`crate::plex::probe::Outcome`] exists
//! to prevent: a final `401` after the parallel direct and relay candidates settle is a TOKEN or
//! access-policy problem, while a refusal is a REACHABILITY problem, and reporting the first as
//! the second sends a user to look at their friend's router. `auth::get_identity` had already had
//! to hand-roll its own
//! open/read/close to keep the two apart; it calls this instead now, and gets the same answer over
//! either transport. The three wrappers had no other callers and went with the change.
//!
//! Callers that genuinely want the fold still write it — `plex::client`'s read choke points check
//! [`Reply::ok`] — but they write it, rather than inheriting it from a transport.
//!
//! ## Two asymmetries worth knowing before you read a failure
//!
//! **Deadlines.** Small API calls take [`net::API`] (8 s connect, 25 s total). Content-dependent
//! PMS reads take [`net::BULK`] instead: the same connect setting, a 1-byte/s-for-30-s low-speed
//! guard, and no whole-transfer timeout, so a healthy large library/art/subtitle response is not
//! cut off at 25 s while a stalled body is still bounded. The connect value is not a promise over
//! a synchronous resolver when `NOSIGNAL` is set; `net::global_init` logs the runtime's
//! `AsynchDNS` feature bit. The plaintext arm cannot select either policy: `stream.rs` compiles in
//! its own 2 s connect and 15 s `SO_RCVTIMEO`.
//!
//! **Redirects.** The plaintext transport returns a 3xx response and never follows it. The TLS
//! arm does the same for PMS requests. This is a correctness rule and a credential boundary:
//! every PMS path carries `X-Plex-Token` in its query string, so an automatic cross-origin follow
//! would give libcurl permission to replay a token-bearing URL outside the origin we selected and
//! verified. The plex.tv account wrappers also keep redirects off because their custom headers
//! carry a token. Only the public, headerless QR-image fetch may follow one: at most five hops,
//! HTTP(S) only, and never from HTTPS down to HTTP.
//!
//! **A short body is reported on one arm only.** `stream.rs` logs a line when a response ends
//! before its `Content-Length` (`note_short_body` — the difference between "the JSON would not
//! parse" and "the server never answered"), and the plaintext arm below calls it; the TLS arm
//! cannot, because libcurl owns that framing and `net.rs` sees only the assembled body. The gap is
//! narrower than it looks: `plex::client::get_json` logs the status, the byte count and serde's own
//! error whenever a 2xx will not parse, over either transport.
use crate::plex::{Origin, Scheme};

/// The verb. Three, because three is what the Plex control plane uses: reads, the body-less
/// `PUT /library/parts/{id}` that selects a track server-side, and the POSTs whose params ride the
/// query string (`/:/timeline`, `/playQueues`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Method {
    Get,
    Put,
    Post,
}

impl Method {
    /// The method token as it goes on the request line.
    fn as_str(self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Put => "PUT",
            Method::Post => "POST",
        }
    }
}

/// One completed HTTP response: the status the server sent, and the bytes it sent with it.
///
/// A `Reply` means the request COMPLETED. A transport failure — refused, unresolvable, timed out,
/// a certificate that would not validate — is the `None` around it, and the two must not be
/// collapsed (see the module doc).
pub(crate) struct Reply {
    pub status: i32,
    pub body: Vec<u8>,
}

impl Reply {
    /// 2xx. The fold `stream.rs`'s one-shot wrappers used to apply for every caller, now written
    /// where it is actually wanted.
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// `Accept: application/json` as one header line, without the CRLF — see [`request`] for the
/// framing rule. PMS answers **XML** for `Accept: */*` or no Accept and only JSON for an explicit
/// `application/json`, and a request that forgets it silently parses to zero items rather than
/// failing (`plex/CLAUDE.md`), so this constant is shared rather than spelled per call site.
pub(crate) const ACCEPT_JSON: &str = "Accept: application/json";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BodyPolicy {
    Api,
    Bulk,
    Probe { max: usize, timeout_s: i32 },
}

/// **The one entry point.** One request to `origin` for `path`, over whichever transport that
/// origin's scheme names.
///
/// `path` is everything after the authority — it must already carry its query string and, for the
/// PMS, its `X-Plex-Token` (appended by `plex::client::with_token`, the one token choke point).
/// `headers` are full `"Name: value"` lines **without** CRLF; this function adds the framing each
/// transport wants, which is the one place the two disagree about the shape of a header.
///
/// `None` is a transport failure. Anything the server actually answered — including a `401` and a
/// `500` — comes back as `Some`.
pub(crate) fn request(origin: &Origin, path: &str, method: Method, headers: &[&str]) -> Option<Reply> {
    request_with(origin, path, method, headers, BodyPolicy::Api)
}

/// A PMS request whose response size is content-dependent. Only the TLS arm differs from
/// [`request`]: it keeps the connect timeout and disables the 25 s whole-transfer deadline.
pub(crate) fn request_bulk(origin: &Origin, path: &str, method: Method, headers: &[&str]) -> Option<Reply> {
    request_with(origin, path, method, headers, BodyPolicy::Bulk)
}

/// A bounded discovery probe. The caller chooses 5 s for a local candidate and 10 s for a remote
/// or relay candidate; this façade carries that policy into either transport without either arm
/// trying to infer locality from an address.
pub(crate) fn request_probe(
    origin: &Origin,
    path: &str,
    method: Method,
    headers: &[&str],
    max_body: usize,
    timeout_s: i32,
) -> Option<Reply> {
    request_with(origin, path, method, headers, BodyPolicy::Probe { max: max_body, timeout_s })
}

fn request_with(
    origin: &Origin,
    path: &str,
    method: Method,
    headers: &[&str],
    body_policy: BodyPolicy,
) -> Option<Reply> {
    match origin.scheme() {
        Scheme::Http => plaintext(origin, path, method, headers, body_policy),
        Scheme::Https => tls(origin, path, method, headers, body_policy),
    }
}

/// The plaintext arm: [`crate::stream`]'s raw socket.
///
/// Open, read `hs_status`, drain, close — the composition the three deleted one-shot wrappers each
/// did a lopped-off version of (module doc), and exactly what `auth::get_identity` used to perform
/// by hand for this same reason. It is the shape that lets both halves of the answer out.
///
/// [`Origin::host`] reaches `http_open` **unbracketed**, which is what `getaddrinfo` wants: the
/// bracketed spelling is URL serialization and resolves nothing. `stream.rs` walks the whole
/// resolved chain and dials either address family, so a plaintext hostname and a plaintext v6
/// literal are both ordinary here — which is why `auth::dial_target` has no shape restriction left
/// in it at all.
fn plaintext(origin: &Origin, path: &str, method: Method, headers: &[&str], body_policy: BodyPolicy) -> Option<Reply> {
    // The raw socket takes ONE `extra` blob, CRLF-terminated per line and CRLF-terminated at the
    // end — it is spliced straight into the request head. An empty header list must produce a null
    // pointer, not an empty string, so the head keeps the exact bytes it always had.
    let extra = (!headers.is_empty()).then(|| {
        let mut s = String::new();
        for h in headers {
            s.push_str(h);
            s.push_str("\r\n");
        }
        s
    });
    let host_c = std::ffi::CString::new(origin.host()).ok()?;
    let path_c = std::ffi::CString::new(path).ok()?;
    let extra_c = extra.and_then(|e| std::ffi::CString::new(e).ok());
    let extra_ptr = extra_c.as_ref().map_or(std::ptr::null(), |c| c.as_ptr());

    let mut hs = crate::stream::http_stream_boxed();
    let opened = match body_policy {
        BodyPolicy::Probe { timeout_s, .. } => crate::stream::http_open_probe(
            &mut *hs,
            host_c.as_ptr(),
            origin.port(),
            path_c.as_ptr(),
            extra_ptr,
            method.as_str(),
            timeout_s.saturating_mul(1000),
        ),
        _ => crate::stream::http_open(
            &mut *hs,
            host_c.as_ptr(),
            origin.port(),
            path_c.as_ptr(),
            extra_ptr,
            method.as_str(),
        ),
    };
    // Read the status BEFORE anything else: a non-2xx open has already closed the socket, and the
    // code survives on the struct (`http_open` says so where it closes). That is the whole reason
    // this arm is a composition rather than a wrapper call.
    let status = crate::stream::hs_status(&*hs);
    let mut body = Vec::new();
    // The two non-positive returns are split rather than folded into one `n <= 0`: -1 is a recv
    // ERROR and 0 a clean end, and `note_short_body` needs them apart to say which ended the
    // transfer. Both still break, so what this function returns is unchanged — a short body is
    // handed back exactly as it is, and only the event log gains a fact.
    let mut recv_err = false;
    let mut overflowed = false;
    if opened == 0 {
        let mut chunk = vec![0u8; 65536];
        loop {
            // Read at most one byte past a ceiling. That one-byte lookahead distinguishes an
            // exactly-full body followed by EOF from a body that is actually too large, without
            // ever extending `body` past the cap.
            let room = match body_policy {
                BodyPolicy::Probe { max, .. } => max.saturating_sub(body.len()),
                BodyPolicy::Api | BodyPolicy::Bulk => chunk.len(),
            };
            let want = match body_policy {
                BodyPolicy::Probe { .. } => chunk.len().min(room.saturating_add(1)),
                BodyPolicy::Api | BodyPolicy::Bulk => chunk.len(),
            };
            let n = crate::stream::http_read(&mut *hs, chunk.as_mut_ptr(), want as i32);
            if n < 0 {
                recv_err = true;
                break;
            }
            if n == 0 {
                break;
            }
            if matches!(body_policy, BodyPolicy::Probe { .. }) && n as usize > room {
                overflowed = true;
                break;
            }
            body.extend_from_slice(&chunk[..n as usize]);
        }
        if !overflowed {
            crate::stream::note_short_body(method.as_str(), path, &hs, recv_err);
        }
    }
    crate::stream::http_close(&mut *hs);
    if overflowed {
        crate::log("http: response exceeded body limit");
        return None;
    }
    // A status of 0 is not something a server sent — it is what `http_open`'s parser leaves when
    // the connection never produced an `HTTP/1.x NNN` line at all, i.e. a transport failure. It
    // must not reach a caller as a "response", because `classify` would read it as `Unreachable`
    // by luck rather than by decision, and `Reply::ok` would read it as a refusal.
    (status != 0).then_some(Reply { status, body })
}

/// The TLS arm: [`crate::net`]'s libcurl.
///
/// The URL is `origin.base()` + `path`, so the authority is the one the origin PARSED — the
/// `plex.direct` name a certificate is issued for, bracketed if it is a v6 literal — and never a
/// pair reassembled from an address. `net` verifies peer and host (`SSL_VERIFYPEER` +
/// `SSL_VERIFYHOST=2`), so a retained public or matched-LAN candidate must authenticate the name
/// plex.tv advertised. Unmatched private-LAN connections on a share are removed earlier by
/// `probe::candidates`; validation could reject a stranger there, but could not refund its 8 s
/// sequential connect setting (subject to the synchronous-resolver caveat in the module doc).
fn tls(origin: &Origin, path: &str, method: Method, headers: &[&str], body_policy: BodyPolicy) -> Option<Reply> {
    let url = format!("{}{}", origin.base(), path);
    let owned: Vec<String> = headers.iter().map(|h| (*h).to_string()).collect();
    // A POST carries a body even when that body is empty — the Plex control plane's POSTs put
    // their params in the query string — while GET and the body-less PUT carry none. `net` turns
    // the second shape into `CURLOPT_CUSTOMREQUEST`.
    let body: Option<&[u8]> = matches!(method, Method::Post).then_some(&[][..]);
    let (timeouts, max_body) = match body_policy {
        BodyPolicy::Api => (crate::net::API, None),
        BodyPolicy::Bulk => (crate::net::BULK, None),
        BodyPolicy::Probe { max, timeout_s } => (
            crate::net::Timeouts {
                connect_s: timeout_s as _,
                total_s: timeout_s as _,
                low_speed_bps: 0,
                low_speed_s: 0,
            },
            Some(max),
        ),
    };
    // PMS redirects are responses, never instructions: the path already carries a token. Keeping
    // `FOLLOWLOCATION` off also makes the TLS arm's 3xx semantics match the plaintext arm.
    let r = crate::net::request(&url, &owned, method.as_str(), body, timeouts, false, max_body)?;
    Some(Reply { status: r.status as i32, body: r.body })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The dispatch is on the SCHEME and on nothing else — not on whether the host looks numeric,
    /// not on the port, not on what the caller thinks it is doing. Asserted through the arms'
    /// observable edge rather than by reading a branch: with no libcurl bound on this host the TLS
    /// arm returns `None` without touching a socket, and the plaintext arm reaches
    /// `stream::http_open` and fails to connect. Both are `None`, so what this really pins is that
    /// neither one PANICS and neither one dials the other's target — the useful half on a machine
    /// that has no PMS.
    #[test]
    fn a_request_is_routed_by_the_origins_scheme() {
        // TEST-NET-1 (RFC 5737): guaranteed unrouted, so nothing can answer either of these.
        let http = Origin::parse("http://192.0.2.1:32400").expect("parses");
        let https = Origin::parse("https://192-0-2-1.hash.plex.direct:32400").expect("parses");
        assert_eq!(http.scheme(), Scheme::Http);
        assert_eq!(https.scheme(), Scheme::Https);

        assert!(request(&http, "/identity", Method::Get, &[ACCEPT_JSON]).is_none());
        assert!(request(&https, "/identity", Method::Get, &[ACCEPT_JSON]).is_none());
    }

    /// The verb tokens are what goes on the request line, and `plex::client::put` and the
    /// `/:/timeline` POST both depend on the exact string. Pinned rather than left to a `Debug`
    /// derive, which would spell them `Get`/`Put`/`Post`.
    #[test]
    fn each_method_names_itself_the_way_http_spells_it() {
        assert_eq!(Method::Get.as_str(), "GET");
        assert_eq!(Method::Put.as_str(), "PUT");
        assert_eq!(Method::Post.as_str(), "POST");
    }

    /// **A `Reply` is a response, not a success.** The fold every caller used to inherit from
    /// `stream.rs`'s one-shot wrappers is a method now, so the one caller that must NOT fold — the
    /// probe, where a 401 is a token problem and not a dead address — can simply read the status.
    #[test]
    fn a_reply_reports_the_status_rather_than_folding_it() {
        let r = |status| Reply { status, body: Vec::new() };
        assert!(r(200).ok() && r(204).ok() && r(299).ok());
        assert!(!r(401).ok(), "the status the whole probe outcome model turns on");
        assert!(!r(301).ok(), "a redirect is a response, not a successful PMS operation");
        assert!(!r(500).ok());
    }

    /// The JSON Accept line carries no CRLF — each transport adds its own framing, and a stray one
    /// here would be a header injection into the plaintext request head and a malformed slist
    /// entry for curl.
    #[test]
    fn the_shared_accept_header_is_a_bare_line() {
        assert_eq!(ACCEPT_JSON, "Accept: application/json");
        assert!(!ACCEPT_JSON.contains('\r') && !ACCEPT_JSON.contains('\n'));
    }

    #[test]
    fn an_identity_ceiling_is_enforced_while_the_socket_is_read() {
        use std::io::{Read, Write};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("address").port();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().expect("accept");
            let mut request = [0u8; 1024];
            let _ = socket.read(&mut request).expect("request");
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nabcde")
                .expect("response");
        });

        let origin = Origin::http("127.0.0.1", port as i32);
        assert!(request_probe(&origin, "/identity", Method::Get, &[ACCEPT_JSON], 4, 1).is_none());
        server.join().expect("server");
    }
}
