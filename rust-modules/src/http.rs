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
/// A `Reply` means the request COMPLETED. The ordinary compatibility entry points put `None`
/// around transport failure; [`request_until_outcome`] instead retains transport and deadline as
/// distinct variants. Neither contract may collapse an HTTP response into either failure.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Reply {
    pub status: i32,
    pub body: Vec<u8>,
}

/// One deadline-bearing request, classified where its transport still knows what ended it.
/// `Response` is any completed HTTP answer, not only a 2xx; neither a later clock read nor JSON
/// parsing is allowed to turn it into `Deadline` or `Transport`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RequestOutcome {
    Response(Reply),
    Deadline,
    Transport,
}

impl RequestOutcome {
    fn response(self) -> Option<Reply> {
        match self {
            Self::Response(reply) => Some(reply),
            Self::Deadline | Self::Transport => None,
        }
    }
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
    Deadline { at: std::time::Instant },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DeadlineOwner {
    Caller,
    Liveness,
}

fn effective_deadline(
    caller: std::time::Instant,
    liveness: std::time::Instant,
) -> (std::time::Instant, DeadlineOwner) {
    if caller <= liveness {
        (caller, DeadlineOwner::Caller)
    } else {
        (liveness, DeadlineOwner::Liveness)
    }
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
pub(crate) fn request(
    origin: &Origin,
    path: &str,
    method: Method,
    headers: &[&str],
) -> Option<Reply> {
    request_with(origin, path, method, headers, BodyPolicy::Api).response()
}

/// A PMS request whose response size is content-dependent. Only the TLS arm differs from
/// [`request`]: it keeps the connect timeout and disables the 25 s whole-transfer deadline.
pub(crate) fn request_bulk(
    origin: &Origin,
    path: &str,
    method: Method,
    headers: &[&str],
) -> Option<Reply> {
    request_with(origin, path, method, headers, BodyPolicy::Bulk).response()
}

/// A small control-plane request inside an already-running transaction reserve. Plaintext composes
/// the caller's absolute projection with a rolling inactivity deadline; complete headers and body
/// bytes renew only that liveness clock. TLS instead composes the remaining reserve with the
/// ordinary 25-second whole-request API cap, which progress does not renew. Neither transport can
/// renew the caller's reserve. The typed result distinguishes an issued HTTP/transport result from
/// the absolute timer which actually fired.
pub(crate) fn request_until_outcome(
    origin: &Origin,
    path: &str,
    method: Method,
    headers: &[&str],
    deadline: std::time::Instant,
) -> RequestOutcome {
    request_with(
        origin,
        path,
        method,
        headers,
        BodyPolicy::Deadline { at: deadline },
    )
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
    request_with(
        origin,
        path,
        method,
        headers,
        BodyPolicy::Probe {
            max: max_body,
            timeout_s,
        },
    )
    .response()
}

fn request_with(
    origin: &Origin,
    path: &str,
    method: Method,
    headers: &[&str],
    body_policy: BodyPolicy,
) -> RequestOutcome {
    if !credential_transport_allowed(origin, path, headers) {
        return RequestOutcome::Transport;
    }
    match origin.scheme() {
        Scheme::Http => plaintext(origin, path, method, headers, body_policy),
        Scheme::Https => tls(origin, path, method, headers, body_policy),
    }
}

fn carries_credential(path: &str, headers: &[&str]) -> bool {
    path.to_ascii_lowercase().contains("x-plex-token=")
        || headers.iter().any(|header| {
            header.split_once(':').is_some_and(|(name, _)| {
                name.trim().eq_ignore_ascii_case("x-plex-token")
                    || name.trim().eq_ignore_ascii_case("authorization")
            })
        })
}

pub(crate) fn credential_transport_allowed_by_policy(
    origin: &Origin,
    path: &str,
    headers: &[&str],
    allow_plaintext_credentials: bool,
) -> bool {
    origin.is_tls() || !carries_credential(path, headers) || allow_plaintext_credentials
}

/// The shared control/media credential boundary. Store builds fail closed on a token-bearing HTTP
/// URL; only a build that explicitly carries the developer-trigger feature may exercise a local
/// plaintext PMS for lab work. The log names neither URL nor token.
pub(crate) fn credential_transport_allowed(origin: &Origin, path: &str, headers: &[&str]) -> bool {
    let allowed = credential_transport_allowed_by_policy(
        origin,
        path,
        headers,
        cfg!(feature = "devtriggers"),
    );
    if !origin.is_tls() && carries_credential(path, headers) {
        static REPORTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if !REPORTED.swap(true, std::sync::atomic::Ordering::Relaxed) {
            if allowed {
                crate::log("security: developer build allows plaintext PMS credentials");
            } else {
                crate::log("security: refused plaintext PMS credentials; HTTPS required");
            }
        }
    }
    allowed
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
fn plaintext(
    origin: &Origin,
    path: &str,
    method: Method,
    headers: &[&str],
    body_policy: BodyPolicy,
) -> RequestOutcome {
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
    let Ok(host_c) = std::ffi::CString::new(origin.host()) else {
        return RequestOutcome::Transport;
    };
    let Ok(path_c) = std::ffi::CString::new(path) else {
        return RequestOutcome::Transport;
    };
    let extra_c = extra.and_then(|e| std::ffi::CString::new(e).ok());
    let extra_ptr = extra_c.as_ref().map_or(std::ptr::null(), |c| c.as_ptr());

    let mut hs = crate::stream::http_stream_boxed();
    let mut deadline_liveness = matches!(body_policy, BodyPolicy::Deadline { .. }).then(|| {
        std::time::Instant::now()
            .checked_add(crate::stream::media_stall_budget())
            .unwrap_or_else(std::time::Instant::now)
    });
    let mut response_status = None;
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
        BodyPolicy::Deadline { at } => {
            if std::time::Instant::now() >= at {
                return RequestOutcome::Deadline;
            }
            let (effective, owner) = deadline_liveness
                .map_or((at, DeadlineOwner::Caller), |liveness| {
                    effective_deadline(at, liveness)
                });
            match crate::stream::http_open_until_result(
                &mut *hs,
                host_c.as_ptr(),
                origin.port(),
                path_c.as_ptr(),
                extra_ptr,
                method.as_str(),
                effective,
            ) {
                Ok(()) => 0,
                Err(crate::stream::HttpOpenError::Status(status)) => {
                    response_status = Some(status);
                    -1
                }
                Err(crate::stream::HttpOpenError::Deadline) => {
                    return match owner {
                        DeadlineOwner::Caller => RequestOutcome::Deadline,
                        DeadlineOwner::Liveness => RequestOutcome::Transport,
                    };
                }
                Err(
                    crate::stream::HttpOpenError::Aborted | crate::stream::HttpOpenError::Transport,
                ) => {
                    return RequestOutcome::Transport;
                }
            }
        }
        _ => crate::stream::http_open(
            &mut *hs,
            host_c.as_ptr(),
            origin.port(),
            path_c.as_ptr(),
            extra_ptr,
            method.as_str(),
        ),
    };
    if opened == 0 && matches!(body_policy, BodyPolicy::Deadline { .. }) {
        // A complete response head is transport progress. Preserve the caller's reserve instant,
        // but begin a fresh ordinary inactivity epoch for the body just as the HLS path and
        // SO_RCVTIMEO do; connect/header latency cannot silently consume the body's watchdog.
        deadline_liveness = Some(
            std::time::Instant::now()
                .checked_add(crate::stream::media_stall_budget())
                .unwrap_or_else(std::time::Instant::now),
        );
    }
    // Read the status BEFORE anything else: a non-2xx open has already closed the socket, and the
    // code survives on the struct (`http_open` says so where it closes). That is the whole reason
    // this arm is a composition rather than a wrapper call.
    let status = response_status.unwrap_or_else(|| crate::stream::hs_status(&*hs));
    let mut body = Vec::new();
    // The two non-positive returns are split rather than folded into one `n <= 0`: -1 is a recv
    // ERROR and 0 a clean end, and `note_short_body` needs them apart to say which ended the
    // transfer. Both still break, so what this function returns is unchanged — a short body is
    // handed back exactly as it is, and only the event log gains a fact.
    let mut recv_err = false;
    let mut overflowed = false;
    let mut deadline_failure = None;
    if opened == 0 {
        let mut chunk = vec![0u8; 65536];
        loop {
            // Read at most one byte past a ceiling. That one-byte lookahead distinguishes an
            // exactly-full body followed by EOF from a body that is actually too large, without
            // ever extending `body` past the cap.
            let room = match body_policy {
                BodyPolicy::Probe { max, .. } => max.saturating_sub(body.len()),
                BodyPolicy::Api | BodyPolicy::Bulk | BodyPolicy::Deadline { .. } => chunk.len(),
            };
            let want = match body_policy {
                BodyPolicy::Probe { .. } => chunk.len().min(room.saturating_add(1)),
                BodyPolicy::Api | BodyPolicy::Bulk | BodyPolicy::Deadline { .. } => chunk.len(),
            };
            let (n, read_deadline_owner) = match body_policy {
                BodyPolicy::Deadline { at } => {
                    let (effective, owner) = deadline_liveness
                        .map_or((at, DeadlineOwner::Caller), |liveness| {
                            effective_deadline(at, liveness)
                        });
                    (
                        crate::stream::http_read_until(
                            &mut *hs,
                            chunk.as_mut_ptr(),
                            want as i32,
                            Some(effective),
                        ),
                        Some(owner),
                    )
                }
                _ => (
                    crate::stream::http_read(&mut *hs, chunk.as_mut_ptr(), want as i32),
                    None,
                ),
            };
            if n < 0 {
                recv_err = true;
                if matches!(body_policy, BodyPolicy::Deadline { .. }) {
                    deadline_failure = Some(if n == crate::stream::HTTP_READ_DEADLINE {
                        match read_deadline_owner.unwrap_or(DeadlineOwner::Caller) {
                            DeadlineOwner::Caller => RequestOutcome::Deadline,
                            DeadlineOwner::Liveness => RequestOutcome::Transport,
                        }
                    } else {
                        RequestOutcome::Transport
                    });
                }
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
            if matches!(body_policy, BodyPolicy::Deadline { .. }) {
                deadline_liveness = Some(
                    std::time::Instant::now()
                        .checked_add(crate::stream::media_stall_budget())
                        .unwrap_or_else(std::time::Instant::now),
                );
            }
        }
        if !overflowed {
            crate::stream::note_short_body(method.as_str(), path, &hs, recv_err);
        }
    }
    let content_length = crate::stream::hs_content_length(&*hs);
    crate::stream::http_close(&mut *hs);
    if let Some(failure) = deadline_failure {
        return failure;
    }
    if overflowed {
        crate::log("http: response exceeded body limit");
        return RequestOutcome::Transport;
    }
    if matches!(body_policy, BodyPolicy::Deadline { .. })
        && opened == 0
        && content_length >= 0
        && (body.len() as i64) < content_length
    {
        return RequestOutcome::Transport;
    }
    // A status of 0 is not something a server sent — it is what `http_open`'s parser leaves when
    // the connection never produced an `HTTP/1.x NNN` line at all, i.e. a transport failure. It
    // must not reach a caller as a "response", because `classify` would read it as `Unreachable`
    // by luck rather than by decision, and `Reply::ok` would read it as a refusal.
    if status == 0 {
        RequestOutcome::Transport
    } else {
        RequestOutcome::Response(Reply { status, body })
    }
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
fn tls(
    origin: &Origin,
    path: &str,
    method: Method,
    headers: &[&str],
    body_policy: BodyPolicy,
) -> RequestOutcome {
    let url = format!("{}{}", origin.base(), path);
    let owned: Vec<String> = headers.iter().map(|h| (*h).to_string()).collect();
    // A POST carries a body even when that body is empty — the Plex control plane's POSTs put
    // their params in the query string — while GET and the body-less PUT carry none. `net` turns
    // the second shape into `CURLOPT_CUSTOMREQUEST`.
    let body: Option<&[u8]> = matches!(method, Method::Post).then_some(&[][..]);
    let (timeouts, max_body, caller_owns_timeout) = match body_policy {
        BodyPolicy::Api => (crate::net::API, None, false),
        BodyPolicy::Bulk => (crate::net::BULK, None, false),
        BodyPolicy::Deadline { at } => {
            let now = std::time::Instant::now();
            if now >= at {
                return RequestOutcome::Deadline;
            }
            let remaining = at.saturating_duration_since(now);
            let reserve_us = remaining.as_micros();
            let reserve_ms = ((reserve_us.saturating_add(999) / 1_000)
                .max(1)
                .min(std::os::raw::c_long::MAX as u128))
                as std::os::raw::c_long;
            let api_ms = crate::net::API
                .total_s
                .saturating_mul(1_000)
                .max(crate::net::API.total_ms)
                .max(1);
            let effective_ms = reserve_ms.min(api_ms);
            // Curl reports both connect and total expiry as code 28. The caller owns that code only
            // when its reserve is no later than BOTH ordinary ceilings; otherwise the issued fact
            // is ambiguous and must remain Transport rather than being relabelled from the clock.
            let ordinary_connect_ms = crate::net::API.connect_s.saturating_mul(1_000).max(1);
            let caller_owns_timeout = reserve_ms <= api_ms && reserve_ms <= ordinary_connect_ms;
            (
                crate::net::Timeouts {
                    connect_s: crate::net::API
                        .connect_s
                        .min((effective_ms.saturating_add(999) / 1_000).max(1)),
                    total_s: 0,
                    total_ms: effective_ms,
                    low_speed_bps: 0,
                    low_speed_s: 0,
                },
                None,
                caller_owns_timeout,
            )
        }
        BodyPolicy::Probe { max, timeout_s } => (
            crate::net::Timeouts {
                connect_s: timeout_s as _,
                total_s: timeout_s as _,
                total_ms: 0,
                low_speed_bps: 0,
                low_speed_s: 0,
            },
            Some(max),
            false,
        ),
    };
    // PMS redirects are responses, never instructions: the path already carries a token. Keeping
    // `FOLLOWLOCATION` off also makes the TLS arm's 3xx semantics match the plaintext arm.
    match crate::net::request_result(
        &url,
        &owned,
        method.as_str(),
        body,
        timeouts,
        false,
        max_body,
    ) {
        Ok(r) => RequestOutcome::Response(Reply {
            status: r.status as i32,
            body: r.body,
        }),
        Err(crate::net::RequestError::TimedOut) if caller_owns_timeout => RequestOutcome::Deadline,
        Err(crate::net::RequestError::TimedOut | crate::net::RequestError::Transport) => {
            RequestOutcome::Transport
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cross(deadline: std::time::Instant) {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if !left.is_zero() {
            std::thread::sleep(left + std::time::Duration::from_millis(10));
        }
        assert!(
            std::time::Instant::now() >= deadline,
            "the fixture did not cross its deadline"
        );
    }

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
        let r = |status| Reply {
            status,
            body: Vec::new(),
        };
        assert!(r(200).ok() && r(204).ok() && r(299).ok());
        assert!(
            !r(401).ok(),
            "the status the whole probe outcome model turns on"
        );
        assert!(
            !r(301).ok(),
            "a redirect is a response, not a successful PMS operation"
        );
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
    fn store_policy_refuses_credentials_over_plaintext_http() {
        let http = Origin::http("127.0.0.1", 32400);
        let https = Origin::parse("https://127-0-0-1.hash.plex.direct:32400").unwrap();
        let token_path = "/identity?X-Plex-Token=secret";

        assert!(!credential_transport_allowed_by_policy(
            &http,
            token_path,
            &[],
            false,
        ));
        assert!(!credential_transport_allowed_by_policy(
            &http,
            "/identity",
            &["Authorization: Bearer secret"],
            false,
        ));
        assert!(credential_transport_allowed_by_policy(
            &https,
            token_path,
            &[],
            false,
        ));
        assert!(credential_transport_allowed_by_policy(
            &http,
            "/identity",
            &[ACCEPT_JSON],
            false,
        ));
        assert!(credential_transport_allowed_by_policy(
            &http,
            token_path,
            &[],
            true,
        ));
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
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nabcde",
                )
                .expect("response");
        });

        let origin = Origin::http("127.0.0.1", port as i32);
        assert!(request_probe(&origin, "/identity", Method::Get, &[ACCEPT_JSON], 4, 1).is_none());
        server.join().expect("server");
    }

    #[test]
    fn a_completed_500_cannot_become_a_deadline_after_the_caller_crosses_the_boundary() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().expect("accept");
            let mut request = [0u8; 2048];
            let _ = socket.read(&mut request).expect("request");
            socket
                .write_all(
                    b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 4\r\nConnection: close\r\n\r\nnope",
                )
                .expect("response");
        });
        let origin = Origin::http("127.0.0.1", port as i32);
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(250);

        let outcome = request_until_outcome(&origin, "/decision", Method::Get, &[], deadline);
        server.join().unwrap();
        cross(deadline);

        assert!(matches!(
            outcome,
            RequestOutcome::Response(Reply { status: 500, ref body }) if body.is_empty()
        ));
    }

    #[test]
    fn a_transport_reset_cannot_become_a_deadline_after_the_caller_crosses_the_boundary() {
        use std::io::Read;
        use std::os::fd::AsRawFd;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().expect("accept");
            let mut request = [0u8; 2048];
            let _ = socket.read(&mut request).expect("request");
            let reset = libc::linger {
                l_onoff: 1,
                l_linger: 0,
            };
            let rc = unsafe {
                libc::setsockopt(
                    socket.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_LINGER,
                    &reset as *const _ as *const std::ffi::c_void,
                    std::mem::size_of::<libc::linger>() as libc::socklen_t,
                )
            };
            assert_eq!(rc, 0, "arm reset-on-close");
        });
        let origin = Origin::http("127.0.0.1", port as i32);
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(250);

        let outcome = request_until_outcome(&origin, "/decision", Method::Get, &[], deadline);
        server.join().unwrap();
        cross(deadline);

        assert!(matches!(outcome, RequestOutcome::Transport));
    }

    #[test]
    fn only_the_timer_that_stops_the_request_is_a_deadline() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().unwrap().port();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let (_silent_peer, _) = listener.accept().expect("accept");
            let _ = release_rx.recv();
        });
        let origin = Origin::http("127.0.0.1", port as i32);
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(80);

        let outcome = request_until_outcome(&origin, "/decision", Method::Get, &[], deadline);
        let _ = release_tx.send(());
        server.join().unwrap();

        assert!(matches!(outcome, RequestOutcome::Deadline));
    }
}
