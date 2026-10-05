//! **The one door out of the control plane** — the single place that decides which transport a
//! Plex REST request takes, and the only place that knows there is more than one.
//!
//! ## Why this file exists
//!
//! The control plane has two request transports and they are not interchangeable:
//!
//! * [`plx_net::stream`] is a raw TCP socket. It resolves through `getaddrinfo` and dials either
//!   address family, it is fast, and it speaks **cleartext only**.
//! * [`plx_net::net`] is libcurl. It also validates certificates, and it is what plex.tv has always
//!   been reached through.
//!
//! Media makes its own scheme decision under `ff.rs`: the same `stream.rs` socket for plaintext,
//! or `crate::curlio`'s interruptible libcurl-multi pull source for HTTPS. That is deliberately
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
use crate::plex::{CredentialPolicy, Origin, Scheme, ResolvePin};

/// The verb. Three, because three is what the Plex control plane uses: reads, the body-less
/// `PUT /library/parts/{id}` that selects a track server-side, and the POSTs whose params ride the
/// query string (`/:/timeline`, `/playQueues`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
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
pub struct Reply {
    pub status: i32,
    pub body: Vec<u8>,
    /// The peer leaf's `CURLOPT_PINNEDPUBLICKEY` string. Present only on the answer to a
    /// [`request_probe_learning_key`] over a strictly verified TLS connection (see
    /// [`plx_net::net::Resp::peer_pin`]); `None` for every other request and always over plaintext.
    pub peer_pin: Option<String>,
}

/// One deadline-bearing request, classified where its transport still knows what ended it.
/// `Response` is any completed HTTP answer, not only a 2xx; neither a later clock read nor JSON
/// parsing is allowed to turn it into `Deadline` or `Transport`.
#[derive(Debug, PartialEq, Eq)]
pub enum RequestOutcome {
    Response(Reply),
    Deadline,
    /// Nothing answered. Carries libcurl's [`plx_net::net::RequestFailure`] when the TLS transport
    /// ran and produced one; `None` from the plaintext transport and from a request refused
    /// before any transport ran.
    Transport(Option<plx_net::net::RequestFailure>),
}

impl RequestOutcome {
    fn response(self) -> Option<Reply> {
        match self {
            Self::Response(reply) => Some(reply),
            Self::Deadline | Self::Transport(_) => None,
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
pub const ACCEPT_JSON: &str = "Accept: application/json";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BodyPolicy {
    Api,
    Bulk,
    Probe { max: usize, timeout_s: i32, learn_pin: bool },
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
#[track_caller]
pub fn request(
    origin: &Origin,
    path: &str,
    method: Method,
    headers: &[&str],
    pin: Option<&ResolvePin>,
) -> Option<Reply> {
    request_with(origin, path, method, headers, BodyPolicy::Api, pin).response()
}

/// A PMS request whose response size is content-dependent. Only the TLS arm differs from
/// [`request`]: it keeps the connect timeout and disables the 25 s whole-transfer deadline.
#[track_caller]
pub fn request_bulk(
    origin: &Origin,
    path: &str,
    method: Method,
    headers: &[&str],
    pin: Option<&ResolvePin>,
) -> Option<Reply> {
    request_with(origin, path, method, headers, BodyPolicy::Bulk, pin).response()
}

/// A small control-plane request inside an already-running transaction reserve. Plaintext composes
/// the caller's absolute projection with a rolling inactivity deadline; complete headers and body
/// bytes renew only that liveness clock. TLS instead composes the remaining reserve with the
/// ordinary 25-second whole-request API cap, which progress does not renew. Neither transport can
/// renew the caller's reserve. The typed result distinguishes an issued HTTP/transport result from
/// the absolute timer which actually fired.
#[track_caller]
pub fn request_until_outcome(
    origin: &Origin,
    path: &str,
    method: Method,
    headers: &[&str],
    deadline: std::time::Instant,
    pin: Option<&ResolvePin>,
) -> RequestOutcome {
    request_with(
        origin,
        path,
        method,
        headers,
        BodyPolicy::Deadline { at: deadline },
        pin,
    )
}

/// A bounded discovery probe. The caller chooses 5 s for a local candidate and 10 s for a remote
/// or relay candidate; this façade carries that policy into either transport without either arm
/// trying to infer locality from an address.
///
/// **Pinned.** `auth::race_batch` builds a [`ResolvePin`] for each `https://…plex.direct`
/// candidate from the address plex.tv advertised beside it (`ResolvePin::for_origin`) and hands it
/// down here — exactly the mechanism data calls already use (the `tls` arm below), now run at the
/// DIAL that decides a winner rather than only after one is already decided. The plaintext arm
/// ignores it: a pin belongs to a TLS name, never to a literal. A candidate whose dashed label does
/// not encode the address it was persisted with gets no pin, and resolves through DNS exactly as
/// before.
///
/// `Err` is a transport failure, carrying libcurl's [`plx_net::net::RequestFailure`] when the TLS
/// arm produced one — the evidence a discovery verdict names per route
/// (`plex::probe::RouteOutcome::of_failure`). `None` from the plaintext arm, which has no code.
#[track_caller]
pub fn request_probe(
    origin: &Origin,
    path: &str,
    method: Method,
    headers: &[&str],
    max_body: usize,
    timeout_s: i32,
    pin: Option<&ResolvePin>,
) -> Result<Reply, Option<plx_net::net::RequestFailure>> {
    probe(origin, path, method, headers, max_body, timeout_s, pin, false)
}

/// [`request_probe`] that also reads the peer's public key off a connection libcurl verified, into
/// [`Reply::peer_pin`] (issue #380, for the offline fallback of #378). The identity probe is its
/// only caller, and only for a candidate that has a [`ResolvePin`] (`auth::get_identity`):
/// reading the chain makes libcurl decode all of it, which no ordinary request should pay, and the
/// key is only worth remembering when the same probe also learns WHICH machine answered. The rules for when a pin is present live on [`plx_net::net::Resp::peer_pin`]; over
/// plaintext it is always `None`.
#[track_caller]
pub fn request_probe_learning_key(
    origin: &Origin,
    path: &str,
    method: Method,
    headers: &[&str],
    max_body: usize,
    timeout_s: i32,
    pin: Option<&ResolvePin>,
) -> Result<Reply, Option<plx_net::net::RequestFailure>> {
    probe(origin, path, method, headers, max_body, timeout_s, pin, true)
}

#[allow(clippy::too_many_arguments)]
#[track_caller]
fn probe(
    origin: &Origin,
    path: &str,
    method: Method,
    headers: &[&str],
    max_body: usize,
    timeout_s: i32,
    pin: Option<&ResolvePin>,
    learn_pin: bool,
) -> Result<Reply, Option<plx_net::net::RequestFailure>> {
    match request_with(
        origin,
        path,
        method,
        headers,
        BodyPolicy::Probe {
            max: max_body,
            timeout_s,
            learn_pin,
        },
        pin,
    ) {
        RequestOutcome::Response(reply) => Ok(reply),
        RequestOutcome::Transport(failure) => Err(failure),
        // A probe carries no caller deadline (`BodyPolicy::Probe`), so this is unreachable; it is
        // a failure without evidence rather than a panic if that ever changes.
        RequestOutcome::Deadline => Err(None),
    }
}

#[track_caller]
fn request_with(
    origin: &Origin,
    path: &str,
    method: Method,
    headers: &[&str],
    body_policy: BodyPolicy,
    pin: Option<&ResolvePin>,
) -> RequestOutcome {
    // This is the single dispatch chokepoint for every PMS/plex.tv REST call, plaintext or TLS.
    // A frame-thread caller landing here blocks on SO_RCVTIMEO (up to 15s) with the HUD frozen; the
    // dev threadcheck watchdog SIGABRTs after 2s instead. `assert_may_block` panics (host tests) or
    // aborts (device, `feature="threadcheck"`) if this runs while `FrameScope` says we are on the
    // frame thread and nothing has explicitly called `allow_blocking` first. A call site that trips
    // this in `cargo test` is doing PMS I/O on the main thread and needs to move to a worker (see
    // `route::decision::try_retranscode` for the pattern) or, if it is a pre-existing, not-yet-split
    // path, wrap the call with `allow_blocking` and say why.
    //
    // `#[track_caller]` runs from every public request function (and `Client`'s transport choke
    // points above them) to this guard, so the report names the route/app caller rather than this
    // line, and the per-(label, site) dedupe keeps one report per caller.
    let _guard =
        plx_base::task::assert_may_block(const { &plx_base::task::BlockingLabel::new("PMS HTTP") });
    if !credential_transport_allowed(origin, path, headers) {
        return RequestOutcome::Transport(None);
    }
    match origin.scheme() {
        // The plaintext arm dials the literal it is given; a pin belongs to a TLS NAME only.
        Scheme::Http => plaintext(origin, path, method, headers, body_policy),
        Scheme::Https => tls(origin, path, method, headers, body_policy, pin),
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

pub fn credential_transport_allowed_by_policy(
    origin: &Origin,
    path: &str,
    headers: &[&str],
    policy: CredentialPolicy,
) -> bool {
    !carries_credential(path, headers) || crate::plex::grant::allowed_under(policy, origin)
}

/// The shared control/media credential boundary: a request that carries a credential reaches the
/// wire only when THE authority (`plex::grant`) says its origin may carry one — TLS, a developer
/// build, or a live consented grant for exactly this plaintext origin. Asked per request, so a
/// revoked grant stops the next request, whoever queued it and whenever. The log names neither
/// URL nor token.
pub fn credential_transport_allowed(origin: &Origin, path: &str, headers: &[&str]) -> bool {
    let allowed = credential_transport_allowed_by_policy(
        origin,
        path,
        headers,
        CredentialPolicy::build(),
    );
    if !origin.is_tls() && carries_credential(path, headers) {
        static REPORTED: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);
        if let Some(line) = plaintext_credential_report(&REPORTED, CredentialPolicy::build(), allowed) {
            plx_base::eventlog::log(line);
        }
    }
    allowed
}

/// The plaintext-credential log line for this outcome, the first time the process meets it —
/// once per OUTCOME, not once per process: a store build meets the refusal before the person
/// consents, and the consented send after it is the line a device check looks for.
fn plaintext_credential_report(
    seen: &std::sync::atomic::AtomicU8,
    policy: CredentialPolicy,
    allowed: bool,
) -> Option<&'static str> {
    let (bit, line) = if policy == CredentialPolicy::AllowPlaintext {
        (1, "security: developer build allows plaintext PMS credentials")
    } else if allowed {
        (2, "security: plaintext PMS credentials sent under a consented grant")
    } else {
        (4, "security: refused plaintext PMS credentials; HTTPS required")
    };
    (seen.fetch_or(bit, std::sync::atomic::Ordering::Relaxed) & bit == 0).then_some(line)
}

/// The plaintext arm: [`plx_net::stream`]'s raw socket.
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
    // end — it is spliced straight into the request head. Control-plane is one-shot: send
    // `Connection: close` so PMS does not wait for a second request on an fd we are about to
    // `http_close`. Media sequential GETs call `stream::http_open` directly and omit the header
    // so the demux socket can reuse. A caller that already named Connection keeps their spelling.
    let extra = {
        let has_connection = headers.iter().any(|h| {
            h.split_once(':')
                .is_some_and(|(name, _)| name.eq_ignore_ascii_case("connection"))
        });
        let mut s = String::new();
        if !has_connection {
            s.push_str("Connection: close\r\n");
        }
        for h in headers {
            s.push_str(h);
            s.push_str("\r\n");
        }
        s
    };
    let Ok(host_c) = std::ffi::CString::new(origin.host()) else {
        return RequestOutcome::Transport(None);
    };
    let Ok(path_c) = std::ffi::CString::new(path) else {
        return RequestOutcome::Transport(None);
    };
    let extra_c = std::ffi::CString::new(extra).ok();
    let extra_ptr = extra_c.as_ref().map_or(std::ptr::null(), |c| c.as_ptr());

    let mut hs = plx_net::stream::http_stream_boxed();
    let mut deadline_liveness = matches!(body_policy, BodyPolicy::Deadline { .. }).then(|| {
        std::time::Instant::now()
            .checked_add(plx_net::stream::media_stall_budget())
            .unwrap_or_else(std::time::Instant::now)
    });
    let mut response_status = None;
    let opened = match body_policy {
        BodyPolicy::Probe { timeout_s, .. } => plx_net::stream::http_open_probe(
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
            match plx_net::stream::http_open_until_result(
                &mut *hs,
                host_c.as_ptr(),
                origin.port(),
                path_c.as_ptr(),
                extra_ptr,
                method.as_str(),
                effective,
                &mut plx_base::checkpoint::NoCheckpoint,
            ) {
                Ok(()) => 0,
                Err(plx_net::stream::HttpOpenError::Status(status)) => {
                    response_status = Some(status);
                    -1
                }
                Err(plx_net::stream::HttpOpenError::Deadline) => {
                    return match owner {
                        DeadlineOwner::Caller => RequestOutcome::Deadline,
                        DeadlineOwner::Liveness => RequestOutcome::Transport(None),
                    };
                }
                // `Stopped` cannot occur: this request has no checkpoint.
                Err(
                    plx_net::stream::HttpOpenError::Aborted
                    | plx_net::stream::HttpOpenError::Stopped
                    | plx_net::stream::HttpOpenError::Transport,
                ) => {
                    return RequestOutcome::Transport(None);
                }
            }
        }
        _ => plx_net::stream::http_open(
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
                .checked_add(plx_net::stream::media_stall_budget())
                .unwrap_or_else(std::time::Instant::now),
        );
    }
    // Read the status BEFORE anything else: a non-2xx open has already closed the socket, and the
    // code survives on the struct (`http_open` says so where it closes). That is the whole reason
    // this arm is a composition rather than a wrapper call.
    let status = response_status.unwrap_or_else(|| plx_net::stream::hs_status(&*hs));
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
                        plx_net::stream::http_read_until(
                            &mut *hs,
                            chunk.as_mut_ptr(),
                            want as i32,
                            Some(effective),
                            &mut plx_base::checkpoint::NoCheckpoint,
                        ),
                        Some(owner),
                    )
                }
                _ => (
                    plx_net::stream::http_read(&mut *hs, chunk.as_mut_ptr(), want as i32),
                    None,
                ),
            };
            if n < 0 {
                recv_err = true;
                if matches!(body_policy, BodyPolicy::Deadline { .. }) {
                    deadline_failure = Some(if n == plx_net::stream::HTTP_READ_DEADLINE {
                        match read_deadline_owner.unwrap_or(DeadlineOwner::Caller) {
                            DeadlineOwner::Caller => RequestOutcome::Deadline,
                            DeadlineOwner::Liveness => RequestOutcome::Transport(None),
                        }
                    } else {
                        RequestOutcome::Transport(None)
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
                        .checked_add(plx_net::stream::media_stall_budget())
                        .unwrap_or_else(std::time::Instant::now),
                );
            }
        }
        if !overflowed {
            plx_net::stream::note_short_body(method.as_str(), path, &hs, recv_err);
        }
    }
    let content_length = plx_net::stream::hs_content_length(&*hs);
    plx_net::stream::http_close(&mut *hs);
    if let Some(failure) = deadline_failure {
        return failure;
    }
    if overflowed {
        plx_base::eventlog::log("http: response exceeded body limit");
        return RequestOutcome::Transport(None);
    }
    if matches!(body_policy, BodyPolicy::Deadline { .. })
        && opened == 0
        && content_length >= 0
        && (body.len() as i64) < content_length
    {
        return RequestOutcome::Transport(None);
    }
    // A status of 0 is not something a server sent — it is what `http_open`'s parser leaves when
    // the connection never produced an `HTTP/1.x NNN` line at all, i.e. a transport failure. It
    // must not reach a caller as a "response", because `classify` would read it as `Unreachable`
    // by luck rather than by decision, and `Reply::ok` would read it as a refusal.
    if status == 0 {
        RequestOutcome::Transport(None)
    } else {
        RequestOutcome::Response(Reply { status, body, peer_pin: None })
    }
}

/// The TLS arm: [`plx_net::net`]'s libcurl.
///
/// The URL is `origin.base()` + `path`, so the authority is the one the origin PARSED — the
/// `plex.direct` name a certificate is issued for, bracketed if it is a v6 literal — and never a
/// pair reassembled from an address. `net` verifies peer and host (`SSL_VERIFYPEER` +
/// `SSL_VERIFYHOST=2`) — with one exception, `net::keypin`'s wrong-clock retry, which replaces the
/// chain-and-date check with a pin on a remembered key and still checks the name — so a retained
/// public or matched-LAN candidate must authenticate the name plex.tv advertised. Unmatched private-LAN connections on a share are removed earlier by
/// `probe::candidates`; validation could reject a stranger there, but could not refund its 8 s
/// sequential connect setting (subject to the synchronous-resolver caveat in the module doc).
///
/// `pin`, when the origin has one, is handed to `net` as a ready `CURLOPT_RESOLVE` entry, so the
/// `plex.direct` name is dialled at the address plex.tv advertised beside it and no resolver is
/// consulted — the whole of offline mode, from this layer's point of view. A pin for a DIFFERENT
/// host than this origin's is ignored: the entry is keyed on the URL's own host, and curl would
/// simply never match it, but refusing to send it keeps the log honest.
#[track_caller]
fn tls(
    origin: &Origin,
    path: &str,
    method: Method,
    headers: &[&str],
    body_policy: BodyPolicy,
    pin: Option<&ResolvePin>,
) -> RequestOutcome {
    let url = format!("{}{}", origin.base(), path);
    let resolve = pin
        .filter(|p| p.host() == origin.host() && p.port() == origin.port())
        .map(plx_net::net::resolve::entry_of);
    let owned: Vec<String> = headers.iter().map(|h| (*h).to_string()).collect();
    // A POST carries a body even when that body is empty — the Plex control plane's POSTs put
    // their params in the query string — while GET and the body-less PUT carry none. `net` turns
    // the second shape into `CURLOPT_CUSTOMREQUEST`.
    let body: Option<&[u8]> = matches!(method, Method::Post).then_some(&[][..]);
    let (timeouts, max_body, caller_owns_timeout) = match body_policy {
        BodyPolicy::Api => (plx_net::net::API, None, false),
        BodyPolicy::Bulk => (plx_net::net::BULK, None, false),
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
            let api_ms = plx_net::net::API
                .total_s
                .saturating_mul(1_000)
                .max(plx_net::net::API.total_ms)
                .max(1);
            let effective_ms = reserve_ms.min(api_ms);
            // Curl reports both connect and total expiry as code 28. The caller owns that code only
            // when its reserve is no later than BOTH ordinary ceilings; otherwise the issued fact
            // is ambiguous and must remain Transport rather than being relabelled from the clock.
            let ordinary_connect_ms = plx_net::net::API.connect_s.saturating_mul(1_000).max(1);
            let caller_owns_timeout = reserve_ms <= api_ms && reserve_ms <= ordinary_connect_ms;
            (
                plx_net::net::Timeouts {
                    connect_s: plx_net::net::API
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
        BodyPolicy::Probe { max, timeout_s, .. } => (
            plx_net::net::Timeouts {
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
    let learn_pin = matches!(body_policy, BodyPolicy::Probe { learn_pin: true, .. });
    match plx_net::net::request_result_evidence(
        &url,
        &owned,
        method.as_str(),
        body,
        timeouts,
        false,
        max_body,
        resolve.as_deref(),
        learn_pin,
    ) {
        Ok(r) => RequestOutcome::Response(Reply {
            status: r.status as i32,
            body: r.body,
            peer_pin: r.peer_pin,
        }),
        Err(failure) if failure.cause == plx_net::net::RequestError::TimedOut && caller_owns_timeout => {
            RequestOutcome::Deadline
        }
        Err(failure) => RequestOutcome::Transport(Some(failure)),
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

    /// What a frame-thread PMS request panics with (host tests), run inside a `FrameScope`.
    fn refused_message(call: impl FnOnce() + std::panic::UnwindSafe) -> String {
        let _frame = plx_base::task::FrameScope::enter();
        let payload = std::panic::catch_unwind(call).expect_err("a frame-thread PMS request is refused");
        payload.downcast_ref::<String>().cloned().expect("a formatted panic")
    }

    /// `main-thread block: PMS HTTP` is deduped per (label, call site), so it only separates the
    /// callers if the site it records is the CALLER's, not `request_with`'s own guard line.
    /// `#[track_caller]` runs from every public request function down to the guard; a request
    /// function that dropped it would report one shared site and hide every caller after the first.
    #[test]
    fn a_frame_thread_pms_request_reports_each_callers_own_site() {
        let origin = Origin::parse("http://192.0.2.1:32400").expect("parses");
        let first_line = line!() + 2;
        let first = refused_message(|| {
            let _ = request(&origin, "/identity", Method::Get, &[], None);
        });
        let second_line = line!() + 2;
        let second = refused_message(|| {
            let _ = request_bulk(&origin, "/identity", Method::Get, &[], None);
        });
        let probe_line = line!() + 2;
        let probed = refused_message(|| {
            let _ = request_probe(&origin, "/identity", Method::Get, &[], 4096, 5, None);
        });
        assert!(first.starts_with("main-thread block: PMS HTTP at "), "{first}");
        assert!(first.ends_with(&format!("http.rs:{first_line}")), "{first}");
        assert!(second.ends_with(&format!("http.rs:{second_line}")), "{second}");
        assert!(probed.ends_with(&format!("http.rs:{probe_line}")), "{probed}");
        assert_ne!(first, second, "two callers of the PMS request path are two recorded sites");
    }

    /// The dispatch is on the SCHEME and on nothing else — not on whether the host looks numeric,
    /// not on the port, not on what the caller thinks it is doing. Asserted through the arms'
    /// observable edge rather than by reading a branch: with no libcurl bound on this host the TLS
    /// arm returns `None` without touching a socket, and the plaintext arm reaches
    /// `stream::http_open` and fails to connect. Both are `None`, so what this really pins is that
    /// neither one PANICS and neither one dials the other's target — the useful half on a machine
    /// that has no PMS.
    /// **The control plane end to end.** An https origin whose name no resolver answers for is
    /// dialled at the pinned address: the loopback listener ACCEPTS a connection (the TLS
    /// handshake against a plaintext listener then fails, which is fine — reaching the socket is
    /// the whole claim, and a DNS failure never reaches one). A pin for a different host is not
    /// sent, so the same request stays undialled.
    #[test]
    fn a_pinned_tls_origin_is_dialled_at_the_pinned_address() {
        let _g = plx_base::testlock::serial();
        if !plx_net::net::global_init() {
            return;
        }
        let Ok(srv) = std::net::TcpListener::bind("127.0.0.1:0") else { return };
        let port = srv.local_addr().unwrap().port();
        srv.set_nonblocking(true).unwrap();
        let accepts = std::sync::atomic::AtomicUsize::new(0);
        let stop = std::sync::atomic::AtomicBool::new(false);
        std::thread::scope(|sc| {
            sc.spawn(|| {
                while !stop.load(std::sync::atomic::Ordering::Acquire) {
                    match srv.accept() {
                        Ok(_) => {
                            accepts.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                        }
                        Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(std::time::Duration::from_millis(1))
                        }
                        Err(_) => break,
                    }
                }
            });
            let origin = Origin::parse(&format!("https://no-such-host.invalid:{port}")).unwrap();
            let pin = ResolvePin::for_test("no-such-host.invalid", port as i32, "127.0.0.1".parse().unwrap());
            let other = ResolvePin::for_test("other.invalid", port as i32, "127.0.0.1".parse().unwrap());
            assert!(request(&origin, "/identity", Method::Get, &[], Some(&other)).is_none());
            assert_eq!(accepts.load(std::sync::atomic::Ordering::Acquire), 0, "a foreign pin is not sent");
            assert!(request(&origin, "/identity", Method::Get, &[], Some(&pin)).is_none(), "TLS against a plaintext listener fails, as it must");
            assert_eq!(accepts.load(std::sync::atomic::Ordering::Acquire), 1, "…but the socket was reached through the pin");
            stop.store(true, std::sync::atomic::Ordering::Release);
        });
    }

    /// **`request_probe` — one of the two discovery-probe entry points; `auth::get_identity`
    /// uses `request_probe_learning_key`, the same request that also reads the peer key, when it
    /// holds a pin, and this one otherwise — carries a pin the same way [`request`] does over TLS, and structurally cannot over plaintext.** `auth::race_batch`
    /// builds a [`ResolvePin`] only for a TLS origin (`ResolvePin::for_origin` refuses anything
    /// else outright), and `request_with`'s `Scheme::Http` arm calls `plaintext(...)`, which has no
    /// `pin` parameter at all — there is no plumbing left for a foreign value to travel through even
    /// if one were built. Proved the same way as the test above, at this entry point instead of
    /// `request`'s: the TLS probe reaches the pinned loopback socket with no resolver involved, and a
    /// plaintext probe against a guaranteed-unrouted TEST-NET-1 (RFC 5737) address, given the SAME
    /// pin pointed at that socket, never reaches it.
    #[test]
    fn request_probe_hands_the_pin_to_the_tls_path_only() {
        let _g = plx_base::testlock::serial();
        if !plx_net::net::global_init() {
            return;
        }
        let Ok(srv) = std::net::TcpListener::bind("127.0.0.1:0") else { return };
        let port = srv.local_addr().unwrap().port();
        srv.set_nonblocking(true).unwrap();
        let accepts = std::sync::atomic::AtomicUsize::new(0);
        let stop = std::sync::atomic::AtomicBool::new(false);
        std::thread::scope(|sc| {
            sc.spawn(|| {
                while !stop.load(std::sync::atomic::Ordering::Acquire) {
                    match srv.accept() {
                        Ok(_) => {
                            accepts.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                        }
                        Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(std::time::Duration::from_millis(1))
                        }
                        Err(_) => break,
                    }
                }
            });
            let tls_origin = Origin::parse(&format!("https://no-such-host.invalid:{port}")).unwrap();
            let pin = ResolvePin::for_test("no-such-host.invalid", port as i32, "127.0.0.1".parse().unwrap());
            assert!(
                request_probe(&tls_origin, "/identity", Method::Get, &[], 4096, 1, Some(&pin)).is_err(),
                "TLS against a plaintext listener fails, as it must"
            );
            assert_eq!(
                accepts.load(std::sync::atomic::Ordering::Acquire),
                1,
                "the TLS probe reached the pinned socket with no resolver"
            );

            let http_origin = Origin::parse("http://192.0.2.1:32400").unwrap();
            let same_pin = ResolvePin::for_test("192.0.2.1", 32400, "127.0.0.1".parse().unwrap());
            assert!(
                request_probe(&http_origin, "/identity", Method::Get, &[], 4096, 1, Some(&same_pin))
                    .is_err(),
                "the unrouted literal never answers, pin or no pin"
            );
            assert_eq!(
                accepts.load(std::sync::atomic::Ordering::Acquire),
                1,
                "a plaintext probe must never be redirected to the pinned socket"
            );
            stop.store(true, std::sync::atomic::Ordering::Release);
        });
    }

    #[test]
    fn a_request_is_routed_by_the_origins_scheme() {
        // A loopback port that was bound a moment ago and is closed again: nothing can answer
        // either of these, and each arm learns that from an immediate ECONNREFUSED. This used to
        // dial TEST-NET-1 (RFC 5737), "guaranteed unrouted" -- which means two connect timeouts
        // (10 s of the suite's wall clock, the longest test in `plx_plex`) and, on a network that
        // does answer for that block, a different failure than the one graded.
        let port = std::net::TcpListener::bind("127.0.0.1:0")
            .expect("bind")
            .local_addr()
            .expect("address")
            .port();
        let http = Origin::http("127.0.0.1", port as i32);
        let https = Origin::parse(&format!("https://127.0.0.1:{port}")).expect("parses");
        assert_eq!(http.scheme(), Scheme::Http);
        assert_eq!(https.scheme(), Scheme::Https);

        assert!(request(&http, "/identity", Method::Get, &[ACCEPT_JSON], None).is_none());
        assert!(request(&https, "/identity", Method::Get, &[ACCEPT_JSON], None).is_none());
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
            peer_pin: None,
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

    /// Answer ONE request with `resp` verbatim, then close; hands back the bound port and the
    /// server thread to join. Local to these tests because `stream`'s own fixture is `cfg(test)`
    /// of a lower layer, which this one's tests will not see once the layers are crates.
    fn one_shot_server(resp: &'static [u8]) -> (u16, std::thread::JoinHandle<()>) {
        use std::io::{Read, Write};
        let srv = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = srv.local_addr().expect("address").port();
        let h = std::thread::spawn(move || {
            if let Ok((mut s, _)) = srv.accept() {
                // Drain the request so the client's send() completes; it arrives in one write.
                let mut req = [0u8; 2048];
                let _ = s.read(&mut req);
                let _ = s.write_all(resp);
            }
        });
        (port, h)
    }

    /// `GET /x` at a loopback port through [`request`], the door the control plane uses.
    fn loopback(port: u16) -> Option<Reply> {
        request(&Origin::http("127.0.0.1", port as i32), "/x", Method::Get, &[], None)
    }

    /// The plaintext arm's composition, end to end against a real socket: the stream primitives'
    /// own tests grade what `http_open`/`http_read` report, and these grade what this arm makes of
    /// it. A chunked body (spelled with no space after the colon) reaches the caller decoded.
    #[test]
    fn the_plaintext_arm_hands_back_a_chunked_body_decoded() {
        let (port, h) = one_shot_server(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding:chunked\r\n\r\n4\r\nabcd\r\n3\r\nefg\r\n0\r\n\r\n",
        );
        let r = loopback(port).expect("a 200 is a response");
        h.join().unwrap();
        assert_eq!(
            (r.status, r.body.as_slice()),
            (200, &b"abcdefg"[..]),
            "the chunk framing was left in the body"
        );
    }

    /// A server that promises 10 bytes and closes after 4 still hands the caller those 4 bytes as
    /// a response. Reporting the short body (`note_short_body`) is observability only; what the
    /// data layer does with a truncated body is decided where it was.
    #[test]
    fn the_plaintext_arm_hands_back_a_truncated_body_as_a_response() {
        let (port, h) = one_shot_server(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nabcd");
        let r = loopback(port).expect("a truncated body is still a body — this must not become None");
        h.join().unwrap();
        assert_eq!((r.status, r.body.as_slice()), (200, &b"abcd"[..]));
    }

    /// **A non-2xx is a RESPONSE.** `http_open` refuses a 401 through its return value and leaves
    /// the code on the stream; this arm reads it before anything else, so the caller sees the
    /// server's answer rather than a transport failure (the collapse `plex::probe::Outcome` exists
    /// to avoid).
    #[test]
    fn the_plaintext_arm_hands_back_a_401_as_a_response() {
        let (port, h) = one_shot_server(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n");
        let r = loopback(port).expect("the server ANSWERED — that is not a transport failure");
        h.join().unwrap();
        assert_eq!(r.status, 401);
        assert!(!r.ok(), "…and it is still not a success");
    }

    /// Nothing listening leaves the stream's status at `0`, and this arm turns that into `None`:
    /// a status no server sent must not reach a caller as a response (`classify` would score it
    /// `Unreachable` by luck rather than by decision, and `Reply::ok` would read it as a refusal).
    #[test]
    fn the_plaintext_arm_answers_none_when_nothing_answers() {
        // Bind and drop, so the port is one nothing is listening on any more.
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
            l.local_addr().expect("address").port()
        };
        assert_eq!(loopback(port), None);
    }

    /// The JSON Accept line carries no CRLF — each transport adds its own framing, and a stray one
    /// here would be a header injection into the plaintext request head and a malformed slist
    /// entry for curl.
    #[test]
    fn the_shared_accept_header_is_a_bare_line() {
        assert_eq!(ACCEPT_JSON, "Accept: application/json");
        assert!(!ACCEPT_JSON.contains('\r') && !ACCEPT_JSON.contains('\n'));
    }

    /// A store build refuses plaintext credentials until the person consents — so a refusal is
    /// normally met FIRST, and the consented send after it must still be said once: the log is
    /// the only evidence a grant carried a token (PLX-NATIVE-10's device check reads it).
    #[test]
    fn each_plaintext_credential_outcome_is_reported_once() {
        let seen = std::sync::atomic::AtomicU8::new(0);
        let store = CredentialPolicy::HttpsOnly;
        assert_eq!(
            plaintext_credential_report(&seen, store, false),
            Some("security: refused plaintext PMS credentials; HTTPS required")
        );
        assert_eq!(plaintext_credential_report(&seen, store, false), None, "once per outcome");
        assert_eq!(
            plaintext_credential_report(&seen, store, true),
            Some("security: plaintext PMS credentials sent under a consented grant"),
            "a consented send after a refusal is a different outcome"
        );
        assert_eq!(plaintext_credential_report(&seen, store, true), None);
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
            CredentialPolicy::HttpsOnly,
        ));
        assert!(!credential_transport_allowed_by_policy(
            &http,
            "/identity",
            &["Authorization: Bearer secret"],
            CredentialPolicy::HttpsOnly,
        ));
        assert!(credential_transport_allowed_by_policy(
            &https,
            token_path,
            &[],
            CredentialPolicy::HttpsOnly,
        ));
        assert!(credential_transport_allowed_by_policy(
            &http,
            "/identity",
            &[ACCEPT_JSON],
            CredentialPolicy::HttpsOnly,
        ));
        assert!(credential_transport_allowed_by_policy(
            &http,
            token_path,
            &[],
            CredentialPolicy::AllowPlaintext,
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
            let n = socket.read(&mut request).expect("request");
            assert!(
                request[..n]
                    .windows(b"Connection: close".len())
                    .any(|w| w.eq_ignore_ascii_case(b"connection: close")),
                "control-plane plaintext still sends Connection: close"
            );
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nabcde",
                )
                .expect("response");
        });

        let origin = Origin::http("127.0.0.1", port as i32);
        assert!(request_probe(&origin, "/identity", Method::Get, &[ACCEPT_JSON], 4, 1, None).is_err());
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

        let outcome = request_until_outcome(&origin, "/decision", Method::Get, &[], deadline, None);
        server.join().unwrap();
        cross(deadline);

        assert!(matches!(
            outcome,
            RequestOutcome::Response(Reply { status: 500, ref body, .. }) if body.is_empty()
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

        let outcome = request_until_outcome(&origin, "/decision", Method::Get, &[], deadline, None);
        server.join().unwrap();
        cross(deadline);

        assert!(matches!(outcome, RequestOutcome::Transport(_)));
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

        let outcome = request_until_outcome(&origin, "/decision", Method::Get, &[], deadline, None);
        let _ = release_tx.send(());
        server.join().unwrap();

        assert!(matches!(outcome, RequestOutcome::Deadline));
    }
}
