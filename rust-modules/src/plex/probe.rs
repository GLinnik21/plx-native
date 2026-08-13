//! **Which addresses of a server are worth dialling, and in what order.** Pure policy: this module
//! builds and ranks candidate URLs from what plex.tv already told us, and does no I/O at all — no
//! socket, no thread, no clock. That is deliberate, and it is what makes the rules below testable
//! on the dev Mac (`cargo test --lib`) rather than only on a television.
//!
//! The rules are not cosmetic. A server hands us several addresses and **at most one of them is
//! reachable from where we are standing**; measured live 2026-08-11 against a real share
//! (`docs/shared-servers.md` §2), two of its three advertised connections cost 8 s and a DNS
//! failure respectively, and the third answered in 115 ms. Picking is the feature.
//!
//! Three rules, each earned:
//!
//! 1. **A `local` connection on a NON-owned server is dropped unless `publicAddressMatches`.**
//!    `Connection.local` means "this address is RFC1918", *not* "you are on that LAN" — the share
//!    advertises the OWNER's `172.20.x.x`. Dialling it from here times out after 8 s, and the worse
//!    outcome is that it succeeds: `172.20.x.x` may well be a *different machine on our own LAN*,
//!    which is a probe that connects, answers, and is the wrong server. `publicAddressMatches` is
//!    the field that means what `local` looks like it means — with it true we really are behind the
//!    same NAT, so the address is ours to use. python-plexapi (`myplex.py`) drops these too.
//! 2. **No plain-HTTP candidate when the owner set `httpsRequired`.** It is their setting; a plain
//!    request to such a server is a refusal, not a connection.
//! 3. **Rank local → remote → relay.** The order every Plex client with an order uses. Relay is a
//!    2 Mbit/s tunnel the server transcodes down to fit: a last resort, never a preference. Inside a
//!    tier, an address this client can actually dial comes first: IPv4 before IPv6, and a numeric
//!    literal before a HOSTNAME (see [`is_numeric_address`] — the transport has no resolver at all,
//!    and the measured share lists an unresolvable internal name ahead of the address that answers).
//!
//! ## What a real prober must still do (this module cannot)
//!
//! - **Verify `machineIdentifier` on the response before accepting a connection.** A candidate that
//!   answered is not the server we asked for — rule 1 explains exactly how a stranger's box answers
//!   a probe. [`ProbePlan::machine_id`] is what the answer must equal; anything else is
//!   [`Outcome::WrongServer`] and the candidate is discarded, not retried. `/identity` is the right
//!   probe path: it is unauthenticated, so it answers 200 to anything — useless as a token test and
//!   perfect as a reachability + identity test.
//! - **Treat `401` as its own state, never as "unreachable"** ([`Outcome::Unauthorized`]). The
//!   `accessToken` is per (user, server) and carries the sharing grant; when it stops working the
//!   answer is to refetch `/api/v2/resources`, not to try the next address — every other address of
//!   that server will fail identically, and reporting "can't reach bx23-ldn" for what is a token
//!   problem sends the user to look at their friend's router.
//!
//! The racing itself (parallel dial, first good wins, cancel the rest) lands with the transport
//! work; it belongs above this file, which stays a function of the resource alone.
use super::account::{Connection, Resource};

/// Where an address sits relative to us. The ranking axis every Plex client agrees on, ordered
/// best-first by declaration so the derived `Ord` *is* the preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Location {
    /// Same LAN — no internet needed, and the only tier that survives the WAN going away.
    Local,
    /// Reached over the internet, directly to the owner's address.
    Remote,
    /// Plex's relay tunnel: ~2 Mbit/s, server-side transcode to fit. Last resort.
    Relay,
}

/// Ordered best-first for THIS app, which is not the same as best-first in general: `stream.rs`
/// speaks plain HTTP to a dotted quad with no DNS and no TLS (`stream.rs:239-253`), so an https
/// `plex.direct` origin cannot be dialled at all until the curl control plane lands
/// (`docs/shared-servers.md` §5 step 6). Ranking https below http keeps the order equal to what can
/// actually connect today; when TLS exists, swapping these two declarations is the whole change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Scheme {
    Http,
    Https,
}

/// One address worth dialling. `url` is a bare origin — scheme, host, port, no trailing slash and
/// no path — so a prober appends `/identity` and a client keeps it as its base.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub url: String,
    pub scheme: Scheme,
    pub location: Location,
    /// The raw host as plex.tv gave it: a dotted quad, a v6 literal, or a hostname. Kept beside
    /// `url` because the current transport wants host and port, not a URL.
    pub address: String,
    pub port: i64,
    pub ipv6: bool,
}

/// Everything a prober needs about one server, and nothing it does not.
pub struct ProbePlan {
    /// `clientIdentifier` — the server's `machineIdentifier`, and the only stable identity it has.
    /// **The probe response must equal this before the connection is accepted** (see the module
    /// doc): rule 1 is a live account of a probe that answers and is the wrong machine.
    pub machine_id: String,
    /// The per-(user, server) `accessToken` that carries the sharing grant — NOT the account token,
    /// which authenticates to plex.tv only and gets a 401 from a share. A secret: never logged.
    pub token: String,
    pub owned: bool,
    /// The machine name ("bx23-ldn") — settings surfaces only.
    pub name: String,
    /// The owner's plex.tv handle ("bamx23"), `None` on our own server. The one string the browsing
    /// UI says about a shared source.
    pub source_title: Option<String>,
    /// In rank order, best first. Empty means the policy refused every advertised address, which is
    /// a decision and not a failure to reach anything — nothing was dialled.
    pub candidates: Vec<Candidate>,
}

/// How a probe of one candidate ended. Spelled out here because the distinction the caller must not
/// collapse is structural, not incidental: only [`Unreachable`](Self::Unreachable) and
/// [`WrongServer`](Self::WrongServer) mean "try the next address".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Answered, and the `machineIdentifier` matched [`ProbePlan::machine_id`].
    Reachable,
    /// Answered, but as a different machine. Discard the candidate; do not retry it.
    WrongServer,
    /// 401 — a token problem, not a reachability problem. Every other address of this server will
    /// answer identically, so stop probing it and refetch `/api/v2/resources`.
    Unauthorized,
    /// No answer: refused, timed out, or unresolvable. The only outcome the next candidate can fix.
    Unreachable,
}

/// Would this connection be dialled at all? Rule 1 lives here, alone, so the reason it exists is
/// readable in one place.
fn is_usable(res: &Resource, c: &Connection) -> bool {
    if c.address.is_empty() || c.port <= 0 {
        return false;
    }
    // A non-owned server's `local` address belongs to the OWNER's LAN unless our public address
    // says otherwise. See the module doc, rule 1 — this line is the whole 8-second timeout.
    if c.local && !res.owned && !res.public_address_matches {
        return false;
    }
    true
}

fn tier(c: &Connection) -> Location {
    // `relay` beats `local` when both are set: a relay connection is never on our LAN, whatever
    // its flags say, and mis-tiering it would rank a 2 Mbit/s tunnel first.
    if c.relay {
        Location::Relay
    } else if c.local {
        Location::Local
    } else {
        Location::Remote
    }
}

/// The scheme a `uri` actually names — read off the string rather than trusting `protocol`, because
/// the `uri` is what we would dial and the two need not agree.
fn scheme_of(uri: &str, protocol: &str) -> Scheme {
    if uri.starts_with("http://") || (uri.is_empty() && protocol == "http") {
        Scheme::Http
    } else {
        Scheme::Https
    }
}

/// A v6 literal has to be bracketed before it can carry a port.
fn host_for_url(address: &str) -> String {
    if address.contains(':') {
        format!("[{address}]")
    } else {
        address.to_string()
    }
}

/// Is this address a NUMERIC literal (v4 or v6) rather than a name that needs resolving?
///
/// The fourth ranking axis, and it exists for the same reason `Scheme` is ranked at all: this app's
/// transport has **no name resolution of any kind** — `stream.rs`'s `http_open` builds a
/// `sockaddr_in` from four decimal octets and there is no `getaddrinfo` in the file — so a hostname
/// candidate cannot be dialled however well it ranks. It is not hypothetical: the share measured on
/// 2026-08-11 advertises a custom internal hostname that does not resolve from here at all, and it
/// is listed BEFORE the public IPv4 that answers. Ranking it above a numeric address spends the
/// first probe slot on a name that cannot resolve.
///
/// A hostname is not DROPPED, because it is the only form that can ever carry TLS validation (an
/// https `plex.direct` origin is a name by construction) — it is merely ranked behind the addresses
/// that can be dialled today, which is exactly the treatment IPv6 already gets.
fn is_numeric_address(a: &str) -> bool {
    a.contains(':') // a v6 literal — colons cannot appear in a hostname
        || (a.split('.').count() == 4 && a.split('.').all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit())))
}

/// Every address of `res` that policy allows, best first.
///
/// Each surviving connection yields its advertised `uri` (verbatim — the `plex.direct` hostname's
/// hash label is the *certificate's* UUID, so it cannot be rebuilt from the machine id, and https to
/// the bare IP fails validation by design), and, unless `httpsRequired`, a synthesized
/// `http://{address}:{port}` twin. **That twin is the point of the whole file**: it is the only
/// candidate the app's current transport can dial, and measured against the real share it is the
/// one that answers.
///
/// A `relay` connection gets no http twin: it is a Plex-operated TLS tunnel, and plain HTTP on it is
/// not a thing that exists — synthesizing one would only spend a probe slot proving that.
pub fn candidates(res: &Resource) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = Vec::new();
    for c in res.connections.iter().filter(|c| is_usable(res, c)) {
        let location = tier(c);
        let ipv6 = c.ipv6 || c.address.contains(':');
        let mut push = |url: String, scheme: Scheme| {
            if scheme == Scheme::Http && res.https_required {
                return; // rule 2
            }
            if out.iter().any(|e| e.url == url) {
                return; // plex.tv can advertise the same origin twice; a probe slot is not free
            }
            out.push(Candidate {
                url,
                scheme,
                location,
                address: c.address.clone(),
                port: c.port,
                ipv6,
            });
        };
        if !c.uri.is_empty() {
            push(c.uri.trim_end_matches('/').to_string(), scheme_of(&c.uri, &c.protocol));
        }
        if !c.relay {
            push(format!("http://{}:{}", host_for_url(&c.address), c.port), Scheme::Http);
        }
    }
    // Stable, so plex.tv's own order survives inside a tier — it is the only tiebreak left once
    // location, scheme, address family and resolvability have spoken, and it is not ours to reorder.
    out.sort_by_key(|c| (c.location, c.scheme, c.ipv6, !is_numeric_address(&c.address)));
    out
}

/// The plan for one server: identity to verify, token to send, addresses to try.
pub fn plan(res: &Resource) -> ProbePlan {
    ProbePlan {
        machine_id: res.client_identifier.clone(),
        token: res.access_token.clone(),
        owned: res.owned,
        name: res.name.clone(),
        source_title: res.source_title.clone(),
        candidates: candidates(res),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two fixtures are the shapes measured live on 2026-08-11 (`docs/shared-servers.md` §2):
    /// addresses and identifiers are stand-ins, the arrangement of flags is not.
    fn parse(json: &str) -> Resource {
        serde_json::from_str(json).expect("fixture parses")
    }

    /// The real share: three advertised connections, exactly one of which works from here.
    /// `local` is the owner's `172.20.x.x` (8 s timeout, and possibly someone else's box on our own
    /// LAN); the custom hostname does not resolve for us; the public IPv4 answered in 115 ms —
    /// **over plain HTTP**, because the owner did not require secure connections.
    fn shared_server() -> Resource {
        parse(
            r#"{"name":"bx23-ldn","clientIdentifier":"bbbb2222","provides":"server","owned":false,
                "sourceTitle":"bamx23","ownerId":987654,"publicAddressMatches":false,
                "httpsRequired":false,"accessToken":"tok-share","connections":[
                  {"protocol":"https","address":"172.20.4.7","port":32400,
                   "uri":"https://172-20-4-7.hash2.plex.direct:32400","local":true,"relay":false,"IPv6":false},
                  {"protocol":"https","address":"media.example.internal","port":26937,
                   "uri":"https://media.example.internal:26937","local":false,"relay":false,"IPv6":false},
                  {"protocol":"https","address":"203.0.113.9","port":26937,
                   "uri":"https://203-0-113-9.hash2.plex.direct:26937","local":false,"relay":false,"IPv6":false}]}"#,
        )
    }

    /// Our own server: two LAN addresses (v4 and v6), a public one, and a relay.
    ///
    /// `publicAddressMatches` is **false**, which is what the live capture actually returned for it
    /// — and it is the flag value that makes this fixture load-bearing. Set it `true` and the
    /// `!res.owned` half of rule 1 is never exercised, because the `publicAddressMatches` clause
    /// keeps the LAN tier on its own; deleting `&& !res.owned` then passes the whole suite while
    /// dropping OUR OWN `192.168.x.x` in the field, taking offline play with it.
    fn owned_server() -> Resource {
        parse(
            r#"{"name":"Gleb's Mac mini","clientIdentifier":"aaaa1111","provides":"server","owned":true,
                "sourceTitle":null,"ownerId":null,"publicAddressMatches":false,"httpsRequired":false,
                "accessToken":"tok-own","connections":[
                  {"protocol":"https","address":"2001:db8::1","port":32400,
                   "uri":"https://2001-db8--1.hash1.plex.direct:32400","local":true,"relay":false,"IPv6":true},
                  {"protocol":"https","address":"192.168.0.10","port":32400,
                   "uri":"https://192-168-0-10.hash1.plex.direct:32400","local":true,"relay":false,"IPv6":false},
                  {"protocol":"https","address":"198.51.100.4","port":32400,
                   "uri":"https://198-51-100-4.hash1.plex.direct:32400","local":false,"relay":false,"IPv6":false},
                  {"protocol":"https","address":"plex-relay.example.net","port":8443,
                   "uri":"https://plex-relay.example.net:8443","local":false,"relay":true,"IPv6":false}]}"#,
        )
    }

    /// The share's `local` connection must not survive policy, and the plain-http twin of its public
    /// IPv4 must — that single candidate is the difference between the feature working and an
    /// 8-second hang followed by "no server found".
    #[test]
    fn the_shares_local_address_is_dropped_and_its_public_ipv4_yields_plain_http() {
        let cs = candidates(&shared_server());

        assert!(
            !cs.iter().any(|c| c.address.starts_with("172.20") || c.url.contains("172-20")),
            "the OWNER's LAN address is not ours to dial: {cs:#?}"
        );
        assert!(
            cs.iter().any(|c| c.url == "http://203.0.113.9:26937" && c.scheme == Scheme::Http),
            "the one connection measured as reachable from the TV: {cs:#?}"
        );
        // two surviving connections, each an https uri + an http twin
        assert_eq!(cs.len(), 4);
        assert!(cs.iter().all(|c| c.location == Location::Remote), "a share has no local tier here");
        // and http outranks https for now, because https is the one this app cannot dial yet
        assert_eq!(cs[0].scheme, Scheme::Http);
    }

    /// **A hostname ranks behind a numeric address**, and the share is the live case: plex.tv lists
    /// the owner's internal name (`media.example.internal`, which does not resolve from here)
    /// BEFORE the public IPv4 that answered in 115 ms. This client's transport has no resolver at
    /// all, so ranking the name first spends the first probe slot proving that.
    ///
    /// It is ranked, not dropped — a name is the only thing TLS can validate, so it has to survive
    /// for the curl transport to use later.
    #[test]
    fn a_hostname_ranks_behind_an_address_that_can_actually_be_dialled() {
        let cs = candidates(&shared_server());
        let http: Vec<&Candidate> = cs.iter().filter(|c| c.scheme == Scheme::Http).collect();

        assert_eq!(http[0].address, "203.0.113.9", "the numeric address leads its tier: {cs:#?}");
        assert_eq!(http[1].address, "media.example.internal", "the name is kept, just not first");
        assert!(
            cs.iter().any(|c| c.address == "media.example.internal" && c.scheme == Scheme::Https),
            "and its https uri survives for the TLS transport: {cs:#?}"
        );

        assert!(is_numeric_address("203.0.113.9") && is_numeric_address("2001:db8::1"));
        assert!(!is_numeric_address("media.example.internal"));
        assert!(!is_numeric_address("203-0-113-9.hash2.plex.direct"), "a plex.direct name is a NAME");
    }

    /// A friend on our own LAN (Plex Home, or a share while visiting) is the case rule 1 must not
    /// break: `publicAddressMatches` says we are behind the same NAT, so the local address is real.
    #[test]
    fn a_non_owned_local_address_survives_when_our_public_address_matches() {
        let mut res = shared_server();
        res.public_address_matches = true;
        let cs = candidates(&res);

        assert_eq!(cs[0].location, Location::Local, "the LAN address now leads: {cs:#?}");
        assert!(cs.iter().any(|c| c.url == "http://172.20.4.7:32400"));
        assert_eq!(cs.len(), 6, "three connections, two candidates each");
    }

    /// The other half of rule 1, stated on its own because the suite once could not see it: OUR
    /// server's LAN address survives even though `publicAddressMatches` is false — which is the
    /// value the live capture returns for it. Ownership is what makes a `local` address ours, and
    /// this is the assertion that fails if `&& !res.owned` is ever "simplified" away.
    #[test]
    fn our_own_lan_address_survives_a_public_address_that_does_not_match() {
        let res = owned_server();
        assert!(res.owned && !res.public_address_matches, "the fixture must carry both flags");

        let cs = candidates(&res);
        assert!(
            cs.iter().any(|c| c.url == "http://192.168.0.10:32400" && c.location == Location::Local),
            "our own LAN address must never be dropped: {cs:#?}"
        );
    }

    /// Local first, relay last — and the relay is https-only, so it contributes exactly one.
    #[test]
    fn our_own_server_ranks_lan_first_and_relay_last() {
        let cs = candidates(&owned_server());

        let tiers: Vec<Location> = cs.iter().map(|c| c.location).collect();
        assert_eq!(
            tiers,
            vec![
                Location::Local,
                Location::Local,
                Location::Local,
                Location::Local,
                Location::Remote,
                Location::Remote,
                Location::Relay
            ],
            "{cs:#?}"
        );
        assert_eq!(cs[0].url, "http://192.168.0.10:32400", "IPv4 before IPv6 inside the tier");
        assert!(!cs[0].ipv6 && cs[1].ipv6, "the v6 twin is next, not first: {cs:#?}");

        let last = cs.last().expect("a relay candidate");
        assert_eq!((last.location, last.scheme), (Location::Relay, Scheme::Https));
        assert_eq!(
            cs.iter().filter(|c| c.location == Location::Relay).count(),
            1,
            "no plain-http twin is synthesized for a TLS tunnel"
        );
        // a v6 literal must be bracketed before it can carry a port
        assert!(cs.iter().any(|c| c.url == "http://[2001:db8::1]:32400"), "{cs:#?}");
    }

    /// The owner's *Require secure connections* is their call, and it removes every http candidate —
    /// including the synthesized twin, which is the only kind this app can currently dial. The
    /// resulting list is honest rather than empty: those servers wait for the TLS transport.
    #[test]
    fn https_required_suppresses_every_http_candidate() {
        let mut res = owned_server();
        res.https_required = true;
        let cs = candidates(&res);

        assert!(cs.iter().all(|c| c.scheme == Scheme::Https), "{cs:#?}");
        assert_eq!(cs.len(), 4, "one per connection, the advertised uri only");
        assert!(cs.iter().all(|c| c.url.starts_with("https://")));

        // and the share, whose one working address is the plain-http twin, loses it too
        let mut share = shared_server();
        share.https_required = true;
        assert!(candidates(&share).iter().all(|c| c.scheme == Scheme::Https));
    }

    /// Addresses that cannot be dialled are not candidates, and a resource with nothing usable
    /// yields an empty list rather than a placeholder to fail on later.
    #[test]
    fn unusable_connections_never_become_candidates() {
        let res = parse(
            r#"{"name":"broken","clientIdentifier":"eeee5555","provides":"server","owned":true,
                "connections":[
                  {"address":"","port":32400,"uri":"https://nowhere:32400","local":true},
                  {"address":"10.0.0.9","port":0,"uri":"","local":true}]}"#,
        );
        assert!(candidates(&res).is_empty(), "no address and no port are both nothing to dial");
    }

    /// The plan carries the identity a probe must check and the token it must send — the two things
    /// that turn "something answered" into "this server answered, and we are allowed in".
    #[test]
    fn the_plan_carries_the_identity_to_verify_and_the_per_server_token() {
        let p = plan(&shared_server());
        assert_eq!(p.machine_id, "bbbb2222", "what the probe response must equal");
        assert_eq!(p.token, "tok-share", "the sharing grant, not the account token");
        assert!(!p.owned);
        assert_eq!(p.name, "bx23-ldn", "the machine name — settings surfaces only");
        assert_eq!(p.source_title.as_deref(), Some("bamx23"), "the handle the rest of the UI says");
        assert_eq!(p.candidates.len(), 4);
    }
}
