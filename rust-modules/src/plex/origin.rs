//! **Where a server is, as one value: scheme + host + port.** The type every layer below the
//! transport passes around instead of a bare `(host, port)` pair.
//!
//! ## Why this type exists at all
//!
//! The app's CONTROL plane can currently reach exactly one shape of server: **plaintext
//! HTTP**. `stream.rs`, which every plex.tv and PMS *query* goes out through, speaks cleartext and
//! has no TLS in it. (The MEDIA plane is no longer bound that way — `crate::curlio` streams a part
//! over https — but a server you cannot query is one you cannot reach an item on.) So
//! `(host, port)` was a complete description of a server,
//! and every layer — the registry, the session file, the login flow, the stream URLs — carried
//! those two values and nothing else.
//!
//! It is not a complete description of the servers plex.tv actually advertises, and the gap is not
//! cosmetic. [`super::probe`] builds ranked candidates from an account's `/api/v2/resources`, and
//! the one field that says where a candidate really is is [`Candidate::url`](super::probe::Candidate::url)
//! — **not** `Candidate::address`. plex.tv advertises the `plex.direct` HOSTNAME in `uri` while
//! `address` stays the dotted quad hiding behind it, and `probe`'s own doc says outright that
//! "https to the bare IP fails validation by design": the certificate is issued for
//! `203-0-113-9.hash.plex.direct`, so a TLS connection made to `203.0.113.9` fails hostname
//! validation however well the packets flow. An origin rebuilt from `address` therefore *looks*
//! like it speaks TLS and cannot: it is exactly the bug this type is here to make unwriteable.
//!
//! **So an `Origin` is PARSED from a URL, never assembled from an address.** [`Origin::parse`] is
//! the front door, [`super::probe::Candidate::origin`] is the one that matters, and
//! [`Origin::http`] is the deliberately-named plaintext constructor — every remaining caller of
//! *that* is a place that still assumes cleartext, and `Origin::http(` is the grep that finds them.
//!
//! ## The bracket rule, which is an invariant and not a detail
//!
//! A v6 address appears in two different spellings and they are not interchangeable:
//!
//! * **`getaddrinfo` takes the BARE literal** — `::1`. Hand it `[::1]` and resolution fails.
//! * **A URL takes the BRACKETED form** — `[::1]:32400`. Without the brackets the port colon is
//!   indistinguishable from an address group, and `http://::1:32400` names nothing.
//!
//! [`Origin::host`] is therefore **always unbracketed** (it is the resolver's node argument) and
//! [`Origin::authority`] is **always bracket-correct** (it is URL serialization). Anything that
//! dials calls `host()`; anything that builds a URL calls `authority()` or [`Origin::base`]. The
//! round trip `http://[::1]:32400` → `host() == "::1"` → `authority() == "[::1]:32400"` →
//! `base() == "http://[::1]:32400"` is pinned by a test, because the failure mode without it is
//! silent: URL construction keeps looking right while name resolution quietly stops working.
//!
//! ## This is a PMS origin parser, not a general URL parser
//!
//! Two deliberate narrowings, both because every origin in this app is a Plex server's:
//!
//! * **The default port is 32400**, not 80/443. It is what `StreamUrl::parse` has always defaulted
//!   to, and a PMS with no port in its URL is a PMS on the default port.
//! * **Only `http` and `https` are recognised.** [`Origin::parse`] refuses anything else rather
//!   than inventing a meaning for it.
use super::probe::dial_port;

/// Which transport an origin names. Lives here rather than in [`super::probe`] because it is a
/// property of an ORIGIN; probe merely *ranks* it, and re-exports this type for that.
///
/// **Ordered best-first FOR THIS APP, which is not best-first in general**, and the derived `Ord`
/// *is* that preference: `stream.rs` speaks plain HTTP — it resolves a name now, but it has no
/// TLS — so an https `plex.direct` origin cannot be QUERIED at all until the curl control plane lands
/// (`docs/shared-servers.md` §5 step 6). Its MEDIA half already works — `crate::curlio` streams a
/// part over https — so this reason is narrower than it was, and the ranking it justifies is
/// unchanged: a server the data layer cannot ask for metadata is one no item can be reached on.
/// Ranking https below http keeps
/// [`super::probe::candidates`]'s order equal to what can actually connect today; when the control
/// plane speaks TLS, swapping these two declarations is the whole change.
///
/// `Default` is [`Scheme::Http`] — the scheme this app has always spoken, and what a session file
/// or a dev trigger written before schemes existed meant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scheme {
    #[default]
    Http,
    Https,
}

impl Scheme {
    /// The wire spelling, and the one used to serialize an origin.
    pub fn as_str(self) -> &'static str {
        match self {
            Scheme::Http => "http",
            Scheme::Https => "https",
        }
    }
    /// Does this scheme require a TLS handshake — and therefore a certificate whose name must
    /// match [`Origin::host`]?
    pub fn is_tls(self) -> bool {
        self == Scheme::Https
    }
}

/// The port a PMS origin means when its URL names none. See the module doc: 32400, not 80/443.
pub const DEFAULT_PORT: i32 = 32400;

/// **One server's address, completely.** Scheme, host and port — no path, no query, no trailing
/// slash. See the module doc for the bracket invariant on [`Origin::host`] vs [`Origin::authority`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    scheme: Scheme,
    /// **Always UNBRACKETED**, even for a v6 literal — this is the `getaddrinfo` node argument.
    host: String,
    /// The port to dial.
    ///
    /// **Read this as "the transport's argument", not as "a validated port".** [`Origin::parse`]
    /// narrows through [`dial_port`], so anything read out of a URL is in `1..=65535` — but
    /// [`Origin::new`] and [`Origin::http`] take the caller's `i32` as given, and one caller has an
    /// `i64` from a file to cast first: `session::ServerRef::origin`'s legacy fallback, which is
    /// total by design and gated by `Session::can_go_local` instead (its doc says why). The
    /// narrowing lives in [`dial_port`] and at the gates, not in this field.
    port: i32,
}

impl Origin {
    /// Build an origin from parts. `host` may arrive **either** spelling — bare or bracketed —
    /// because a caller holding a v6 address has usually just read it out of a URL authority; it
    /// is stored bare regardless, which is the invariant [`Origin::host`] promises. Unbalanced
    /// brackets are left alone rather than half-stripped.
    pub fn new(scheme: Scheme, host: &str, port: i32) -> Origin {
        Origin { scheme, host: unbracket(host).to_owned(), port }
    }

    /// The **plaintext** constructor, named so that it is greppable.
    ///
    /// Every call is a place that still assumes cleartext — the legacy `(host, port)` registry
    /// entry points, the compiled-in PMS host, a session file with no stored origin. Each one is a
    /// line the TLS lane has to revisit, which is why they are spelled `Origin::http(` rather than
    /// `Origin::new(Scheme::Http, …)`. This used to add "that is correct today (there is no TLS
    /// transport)", which stopped being true of the MEDIA plane when `crate::curlio` landed: an
    /// origin invented here that ends up in a part URL now silently downgrades a server that could
    /// have been streamed from securely.
    pub fn http(host: &str, port: i32) -> Origin {
        Origin::new(Scheme::Http, host, port)
    }

    /// Parse an origin out of a URL — **the front door**, and the only way an https origin can
    /// ever come into existence, because the hostname TLS validates against exists nowhere but the
    /// URL (module doc).
    ///
    /// `None` when there is no host, when the scheme is one this app does not speak, or when a
    /// port is written and is not dialable ([`dial_port`]). A path is ignored: an origin is the
    /// prefix, and [`split`] is the entry point for a caller that wants the rest of the URL too.
    ///
    /// A MISSING scheme is read as `http`. That is not laxity for its own sake — it is what
    /// `StreamUrl::parse` has always done with the `/tmp/plxnative-url` override, and this
    /// function replaced that parser.
    pub fn parse(url: &str) -> Option<Origin> {
        let p = Parts::of(url);
        (p.scheme_known && !p.host.is_empty() && !p.port_bad)
            .then(|| Origin::new(p.scheme, p.host, p.port.unwrap_or(DEFAULT_PORT)))
    }

    pub fn scheme(&self) -> Scheme {
        self.scheme
    }
    /// Does reaching this origin need a TLS handshake? The one question a transport asks.
    pub fn is_tls(&self) -> bool {
        self.scheme.is_tls()
    }
    /// **The resolver's node argument — never bracketed**, even for a v6 literal. Also the name a
    /// TLS certificate must match. See the module doc.
    pub fn host(&self) -> &str {
        &self.host
    }
    pub fn port(&self) -> i32 {
        self.port
    }
    /// **URL serialization — `host:port`, bracketing a v6 literal.** The other half of the
    /// bracket invariant: this is what goes in a URL, `host()` is what goes to the resolver.
    pub fn authority(&self) -> String {
        if is_v6_literal(&self.host) {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
    /// `"{scheme}://{authority}"` — no trailing slash, no path. Round-trips through
    /// [`Origin::parse`], and is the form persisted in the session file.
    pub fn base(&self) -> String {
        format!("{}://{}", self.scheme.as_str(), self.authority())
    }

    /// **How an origin is written in the EVENT LOG** — the authority alone for a plaintext origin,
    /// the whole base URL for anything else.
    ///
    /// The asymmetry is not cosmetic and it is not indecision. The log has said `host:port` since
    /// before origins existed, and it is the file users paste into issues and the surface every
    /// headless lane grades a run by — so a plaintext registration must keep reading exactly as it
    /// always has, or every archived log becomes incomparable with a current one.
    ///
    /// The other half is the `[[silent-instrument-trap]]` this project has already paid for once:
    /// `dev::DevServer`'s `scheme` field exists *specifically* so a lane with no TLS server of its
    /// own can put an https origin through the registry, and an instrument that cannot see the one
    /// thing it was armed to test is worse than no instrument. Printing the authority for both
    /// would make an https run byte-identical to an http one.
    ///
    /// It carries no path, no query and no token — the same redaction the address-only lines it
    /// replaces already observed.
    pub fn log_form(&self) -> String {
        if self.scheme == Scheme::Http {
            self.authority()
        } else {
            self.base()
        }
    }
}

/// Split a URL into its origin and the rest of it (path + query, `""` when there is none).
///
/// **Total**, unlike [`Origin::parse`], because its caller is `StreamUrl::parse` — which turns the
/// `/tmp/plxnative-url` override into something to dial and has no failure path to take. Every
/// degenerate input therefore yields *something*: a missing scheme reads as `http`, an unknown one
/// is discarded rather than mistaken for a host, and a port that is absent or undialable becomes
/// [`DEFAULT_PORT`] rather than wrapping into a port nobody wrote down (which is
/// [`dial_port`]'s reason for existing, one step downstream).
pub fn split(url: &str) -> (Origin, &str) {
    let p = Parts::of(url);
    (Origin::new(p.scheme, p.host, p.port.unwrap_or(DEFAULT_PORT)), p.path)
}

/// The HOST of a URL, **bare** — a borrowed view for a caller that only wants to look at the host
/// and has no use for an owned [`Origin`].
///
/// Its one caller is [`super::probe`]'s ranking, which asks whether a candidate's host is a numeric
/// literal or a name that needs resolving — and it must ask that of the URL's host and **not** of
/// [`Candidate::address`](super::probe::Candidate::address), which is the dotted quad hiding behind
/// a `plex.direct` NAME. This lives here so that there is exactly one reading of a URL in the
/// codebase; probe used to carry a second copy of it.
pub fn url_host(url: &str) -> &str {
    Parts::of(url).host
}

/// The raw reading of a URL, before any policy is applied to it. Two callers with two different
/// tolerances ([`Origin::parse`] refuses what [`split`] papers over), so the reading and the
/// judgement are separate.
struct Parts<'a> {
    scheme: Scheme,
    /// `false` when the URL named a scheme that is neither `http` nor `https`.
    scheme_known: bool,
    /// Unbracketed already.
    host: &'a str,
    /// `None` when the URL wrote no port at all.
    port: Option<i32>,
    /// `true` when a `:` was written and what follows it is not a dialable port — the case that
    /// must not silently become [`DEFAULT_PORT`] for a caller that can refuse.
    ///
    /// **An EMPTY port counts.** `http://192.0.2.10:` is a truncated write or a half-finished hand
    /// edit, not a request for the default, and reading it as one is exactly the "dial a port
    /// nobody wrote down" outcome [`dial_port`] exists to prevent. RFC 3986 would let an empty port
    /// mean the default; this is a PMS origin parser reading a file that can be corrupt, and the
    /// two want opposite answers.
    port_bad: bool,
    path: &'a str,
}

impl<'a> Parts<'a> {
    fn of(url: &'a str) -> Parts<'a> {
        let (scheme, scheme_known, rest) = match url.split_once("://") {
            Some(("http", r)) => (Scheme::Http, true, r),
            Some(("https", r)) => (Scheme::Https, true, r),
            // A scheme we do not speak. The authority is still readable, so `split` can go on;
            // `parse` refuses on `scheme_known`.
            Some((_, r)) => (Scheme::Http, false, r),
            None => (Scheme::Http, true, url),
        };
        // The authority ends at the first `/` — everything from there is the path. A `?` with no
        // `/` before it is not a shape any URL this app builds takes, and treating it as part of
        // the host would be no worse than treating it as a path.
        let (auth, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => (rest, ""),
        };
        // A bracketed v6 literal carries its own colons, so the port separator is the one AFTER
        // the closing bracket. `probe` used to carry its own copy of this reading, which returned
        // the BRACKETED spelling — right for an authority, wrong for the resolver node this
        // produces, and precisely the confusion the module doc's bracket rule exists to end.
        let (host, port_txt) = match auth.strip_prefix('[').and_then(|r| r.split_once(']')) {
            Some((h, after)) => (h, after.strip_prefix(':')),
            None => match auth.split_once(':') {
                Some((h, p)) => (h, Some(p)),
                None => (auth, None),
            },
        };
        // `dial_port`, not a bare `parse::<i32>()`: this is the same narrowing every advertised
        // port in this layer goes through, and it is what stops `4294999696` from wrapping to a
        // plausible-looking 32400.
        let port = port_txt.and_then(|t| t.parse::<i64>().ok()).and_then(dial_port);
        Parts {
            scheme,
            scheme_known,
            host,
            port,
            port_bad: port_txt.is_some() && port.is_none(),
            path,
        }
    }
}

/// Strip the brackets a URL authority puts around a v6 literal, if they are there.
fn unbracket(host: &str) -> &str {
    host.strip_prefix('[').and_then(|r| r.strip_suffix(']')).unwrap_or(host)
}

/// Is this bare host a v6 literal — i.e. does it need bracketing before it can carry a port?
/// A colon cannot appear in a hostname or in a dotted quad, so one colon is the whole test.
fn is_v6_literal(host: &str) -> bool {
    host.contains(':')
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The bracket invariant, as a round trip.** `host()` is the resolver's node and is bare;
    /// `authority()` is URL serialization and is bracketed; `base()` reproduces the input exactly.
    ///
    /// Without this the failure is silent in the worst direction: every URL the app *builds* keeps
    /// looking correct while `getaddrinfo` is handed `[::1]` and quietly resolves nothing.
    #[test]
    fn a_v6_origin_is_bare_for_the_resolver_and_bracketed_for_a_url() {
        let o = Origin::parse("http://[::1]:32400").expect("a bracketed v6 origin parses");
        assert_eq!(o.host(), "::1", "the getaddrinfo node is NEVER bracketed");
        assert_eq!(o.authority(), "[::1]:32400", "…and a URL authority always is");
        assert_eq!(o.base(), "http://[::1]:32400", "so the round trip is byte-identical");
        assert_eq!(o.port(), 32400);

        // and the same holds for an origin built from parts, whichever spelling the caller has
        assert_eq!(Origin::http("[2001:db8::1]", 32400), Origin::http("2001:db8::1", 32400));
        assert_eq!(Origin::http("2001:db8::1", 32400).authority(), "[2001:db8::1]:32400");
        assert_eq!(Origin::http("2001:db8::1", 32400).base(), "http://[2001:db8::1]:32400");
    }

    /// A v4 origin is the case every line of this app takes today, and it must serialize to
    /// exactly the bytes it always has — no brackets, no default port appearing from nowhere.
    #[test]
    fn a_v4_origin_round_trips_unchanged() {
        let o = Origin::parse("http://192.0.2.10:32400").expect("parses");
        assert_eq!((o.scheme(), o.host(), o.port()), (Scheme::Http, "192.0.2.10", 32400));
        assert_eq!(o.authority(), "192.0.2.10:32400");
        assert_eq!(o.base(), "http://192.0.2.10:32400");
        assert!(!o.is_tls());
    }

    /// **The whole reason this type is parsed and not assembled**: an https `plex.direct` origin
    /// keeps the NAME, which is what the certificate is issued for. Rebuilding it from the dotted
    /// quad behind that name gives a URL that connects and fails validation.
    #[test]
    fn an_https_origin_keeps_the_name_tls_validates_against() {
        let o = Origin::parse("https://203-0-113-9.hash.plex.direct:31234").expect("parses");
        assert_eq!(o.scheme(), Scheme::Https);
        assert!(o.is_tls());
        assert_eq!(o.host(), "203-0-113-9.hash.plex.direct");
        assert_eq!(o.base(), "https://203-0-113-9.hash.plex.direct:31234");
    }

    /// The defaults, each one a behaviour `StreamUrl::parse` has always had and must keep.
    #[test]
    fn a_missing_scheme_is_http_and_a_missing_port_is_the_pms_default() {
        let o = Origin::parse("192.0.2.10").expect("a bare host is an origin");
        assert_eq!((o.scheme(), o.host(), o.port()), (Scheme::Http, "192.0.2.10", DEFAULT_PORT));
        assert_eq!(Origin::parse("https://nas.example.com").unwrap().port(), DEFAULT_PORT);
    }

    /// A path is not part of an origin — `base()` never grows one, whatever came in.
    #[test]
    fn a_path_is_not_part_of_the_origin() {
        assert_eq!(Origin::parse("http://192.0.2.10:32400/library/sections?x=1").unwrap().base(), "http://192.0.2.10:32400");
        assert_eq!(Origin::parse("http://192.0.2.10:32400/").unwrap().base(), "http://192.0.2.10:32400");
    }

    /// What [`Origin::parse`] refuses. Each of these has a caller that would otherwise dial
    /// something nobody named.
    #[test]
    fn parse_refuses_what_cannot_be_dialled() {
        assert_eq!(Origin::parse(""), None, "no host at all");
        assert_eq!(Origin::parse("http://"), None);
        assert_eq!(Origin::parse("ftp://h:21"), None, "a scheme this app does not speak");
        // The port narrowing that `dial_port` exists for: plex.tv and the session file both
        // string-encode numbers, so an out-of-range one is a shape that really arrives — and
        // `4_294_999_696 as i32` is 32400, a port nobody advertised.
        assert_eq!(Origin::parse("http://192.0.2.10:4294999696"), None);
        assert_eq!(Origin::parse("http://192.0.2.10:0"), None);
        assert_eq!(Origin::parse("http://192.0.2.10:70000"), None);
        assert_eq!(Origin::parse("http://192.0.2.10:abc"), None);
        // an EMPTY port is a port that WAS written — a truncated write, not a request for the
        // default. Reading it as 32400 is the same "port nobody wrote down" this list exists for.
        assert_eq!(Origin::parse("http://192.0.2.10:"), None);
        // …while a URL that writes no `:` at all really does mean the default
        assert_eq!(Origin::parse("http://192.0.2.10").unwrap().port(), DEFAULT_PORT);
    }

    /// [`split`] is the TOTAL sibling — its caller has no failure path — so the same inputs that
    /// [`Origin::parse`] refuses still come back as something, with the port defaulted rather than
    /// wrapped.
    #[test]
    fn split_is_total_and_hands_back_the_path() {
        let (o, p) = split("http://192.0.2.10:32400/video/:/transcode/universal/start.mkv?a=1");
        assert_eq!(o.base(), "http://192.0.2.10:32400");
        assert_eq!(p, "/video/:/transcode/universal/start.mkv?a=1");

        assert_eq!(split("192.0.2.10").0.base(), "http://192.0.2.10:32400");
        assert_eq!(split("192.0.2.10").1, "", "no path is an empty path, not a slash");
        assert_eq!(split("http://192.0.2.10:70000/x").0.port(), DEFAULT_PORT, "undialable → the default");
        assert_eq!(split("http://192.0.2.10:/x").0.port(), DEFAULT_PORT, "…and so does an empty one, here");
        assert_eq!(split("http://192.0.2.10:/x").1, "/x");
        assert_eq!(split("https://[2001:db8::1]:8443/y").0.base(), "https://[2001:db8::1]:8443");
        assert_eq!(split("https://[2001:db8::1]:8443/y").1, "/y");
    }

    /// **The event log's spelling**: unchanged for the plaintext origins it has always carried,
    /// and legible the moment a scheme is worth saying. Both halves matter — see [`Origin::log_form`].
    #[test]
    fn the_log_form_is_the_bare_authority_for_http_and_the_whole_url_for_anything_else() {
        assert_eq!(Origin::http("192.0.2.10", 32400).log_form(), "192.0.2.10:32400", "byte-identical to the old line");
        assert_eq!(Origin::parse("https://nas.hash.plex.direct:32400").unwrap().log_form(), "https://nas.hash.plex.direct:32400");
        // a v6 literal is bracketed either way — it is a URL authority, not a resolver node
        assert_eq!(Origin::http("2001:db8::1", 32400).log_form(), "[2001:db8::1]:32400");
    }

    /// [`url_host`] is the borrowed reading `probe`'s ranking uses, and it must give the URL's own
    /// host — the `plex.direct` NAME — not the address behind it.
    #[test]
    fn url_host_reads_the_urls_own_host_without_scheme_or_port() {
        assert_eq!(url_host("http://203.0.113.9:31234"), "203.0.113.9");
        assert_eq!(url_host("https://media.example.internal:31234"), "media.example.internal");
        assert_eq!(url_host("http://[2001:db8::1]:32400"), "2001:db8::1", "the port is not a v6 group");
        assert_eq!(url_host("https://203-0-113-9.hash2.plex.direct:31234"), "203-0-113-9.hash2.plex.direct");
    }

    /// The scheme's wire spelling is what `base()` is built from and what a dev trigger writes,
    /// so it is pinned rather than left to a `Debug` derive.
    #[test]
    fn a_scheme_knows_its_wire_spelling_and_whether_it_needs_tls() {
        assert_eq!((Scheme::Http.as_str(), Scheme::Https.as_str()), ("http", "https"));
        assert!(!Scheme::Http.is_tls() && Scheme::Https.is_tls());
        assert_eq!(Scheme::default(), Scheme::Http, "the scheme this app has always spoken");
        assert!(Scheme::Http < Scheme::Https, "probe RANKS on this order — http is what can be dialled today");
    }
}
