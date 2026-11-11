//! The URI dev route (`plxnative-uridev`): the PRODUCTION URI backend, ACB-bound, on a webOS 4.x
//! set, playing a URL a developer names. Debug builds only: this file is compiled with
//! `devtriggers` and `play-uri` (see `dev.rs`), so a release binary carries none of it.
//!
//! **What it is.** Production certifies no source: its evidence is `ProductEvidence::unqualified_on`
//! (nothing certified) and its qualifier is `ProductQualifier` (`Unknown` unless the viewer opted in to
//! the URI engine in Settings, and then only for a file Standard would direct-play: its video, Dolby Vision, audio
//! and container verdicts, not the server's own decision), so a real resolve of a viewer who has not opted in never
//! offers the URI backend. This module is the one place that composes the other
//! answer for a test run: [`compose`] swaps in dev evidence (the four capabilities a URL carrier
//! needs, certified) and a dev [`OriginalQualifier`] that answers a certified `ProgressiveUri`
//! verdict whose source is the trigger's URL. Everything else is the real route: the resolve
//! worker, `ResolvePort::qualify`'s own checks, the plan landing, the offer, the backend policy,
//! `UriBackend`, the ACB bind. The mock PMS only carries the plan.
//!
//! **Grammar.** `acb part=<N> pms=<dotted-quad:port> url=<http://dotted-quad:port/path>`: the mode
//! word first, then the three keys in any order, space separated. A missing, unknown or repeated
//! key, a bad value, or a URL with a query, a fragment, a space or a hostname refuses the whole
//! trigger, and [`compose`] logs why (never the URL or the address: the URL is opaque and a LAN
//! address is a household's).
//!
//! **Why the server is pinned by ORIGIN.** `selection.server` is only a process-local registry slot,
//! and the PMS's `machineIdentifier` is not there to compare: a dev-token boot (the harness's
//! `--guest --mock`) registers its primary by ADDRESS ONLY (`session::auth::install_captured_registry`
//! calls `register("", ...)`), and nothing learns the id before the first play. What such a slot
//! always has is the origin it was registered at, `http://PMS_HOST:PMS_PORT` verbatim, which is also
//! the address the harness's mock listens on, so `pms=` is that origin and the request carries the
//! client's own ([`OriginalRequest::server_origin`]).
//!
//! **Safety.** A trigger file left in the shared `/tmp` by an aborted run must never certify
//! whatever title a later dev boot on a real account opens, and the production path reports
//! timelines (a write to a real Plex account). So the qualifier answers only for the one Part
//! (`part=`) on the one server (`pms=`, compared as an [`Origin`] with the request's, scheme
//! included) and answers `Unknown` for everything else, another origin included. The two pins are
//! independent: a Part id is the PMS's own number and an origin says where a server is, so neither
//! one alone names a title on a real account the way both together name the harness's carrier on
//! its mock. The qualifier is a plain value: no IO, no global, no clock but the expiry media's
//! `uri_source_expiry_after` stamps.
//!
//! **Compile-away.** The whole file is behind the `devtriggers` feature, so no statement here
//! needs a cfg of its own; the one statement outside it (the call in `adapters::backends`) has
//! one. The log prefix is `uri-dev:`, and every refusal contains `ignored`, the word the
//! harness's `no_error` grade keys on.

use plx_media::route::uri_preflight::{KnownSource, OriginalQualifier, OriginalRequest,
    ProductEvidence, UriDecline, Verdict};
use plx_plex::plex::Origin;
use plx_play_uri::backend::Binding;
use plx_playback_contract::Capabilities;
use std::sync::Arc;
use std::time::Duration;

/// How long a dev verdict stays good: it must outlive the plan landing and the staged driver
/// source (the driver's own TTL is 30 s), as the product qualifier's would.
const VERDICT_TTL: Duration = Duration::from_secs(30);

/// A parsed trigger: the one Part on the one server (by origin) the dev route may certify, and the
/// URL it certifies for it.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Trigger {
    part: i64,
    pms: Origin,
    url: String,
}

/// Why a trigger was refused. The text never carries any of the trigger's own content.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Refusal {
    NotAcb,
    Missing(&'static str),
    UnknownKey,
    Repeated,
    NotKeyValue,
    BadPart,
    BadPms,
    NotHttp,
    HostNotDottedQuad,
    NoPort,
    BadPort,
    NoPath,
    QueryOrFragment,
    BadCharacter,
    NotAcbDisplay,
}

impl Refusal {
    /// The reason a log line gives.
    pub(crate) fn reason(self) -> &'static str {
        match self {
            Refusal::NotAcb => "the first word is not acb",
            Refusal::Missing("part") => "part is missing",
            Refusal::Missing("pms") => "pms is missing",
            Refusal::Missing(_) => "url is missing",
            Refusal::UnknownKey => "unknown key",
            Refusal::Repeated => "a key is given twice",
            Refusal::NotKeyValue => "a word is not key=value (a space in the url?)",
            Refusal::BadPart => "part is not a positive number",
            Refusal::BadPms => "pms is not a dotted-quad:port",
            Refusal::NotHttp => "url is not http",
            Refusal::HostNotDottedQuad => "url host is not a dotted quad",
            Refusal::NoPort => "url has no port",
            Refusal::BadPort => "url port is not 1-65535",
            Refusal::NoPath => "url has no path",
            Refusal::QueryOrFragment => "url has a query or fragment",
            Refusal::BadCharacter => "url has a character the dev route does not take",
            Refusal::NotAcbDisplay => "display is not ACB",
        }
    }
}

/// What a boot's trigger and display amount to.
#[derive(Clone, PartialEq, Eq)]
pub(crate) enum Decision {
    /// No trigger: nothing to say, nothing to change.
    Unarmed,
    Ignored(Refusal),
    Armed(Trigger),
}

impl Decision {
    /// The one log line this decision owes, if any. Every refusal matches `uri-dev: .* ignored`.
    pub(crate) fn line(&self) -> Option<String> {
        match self {
            Decision::Unarmed => None,
            Decision::Ignored(why) => Some(format!("uri-dev: trigger ignored ({})", why.reason())),
            Decision::Armed(trigger) => Some(format!("uri-dev: armed part={}", trigger.part)),
        }
    }
}

/// The trigger text, read once at boot by the composition.
pub(crate) fn trigger() -> Option<String> {
    plx_base::devtrig::read("uridev")
}

/// Parse a trigger's text: the mode word `acb`, then `part=`, `pms=` and `url=` once each, in any
/// order.
pub(crate) fn parse(text: &str) -> Result<Trigger, Refusal> {
    let mut words = text.split_ascii_whitespace();
    if words.next() != Some("acb") {
        return Err(Refusal::NotAcb);
    }
    let (mut part, mut pms, mut url) = (None, None, None);
    for word in words {
        let (key, value) = word.split_once('=').ok_or(Refusal::NotKeyValue)?;
        let slot = match key {
            "part" => &mut part,
            "pms" => &mut pms,
            "url" => &mut url,
            _ => return Err(Refusal::UnknownKey),
        };
        if slot.replace(value).is_some() {
            return Err(Refusal::Repeated);
        }
    }
    let part = part.ok_or(Refusal::Missing("part"))?;
    let pms = pms.ok_or(Refusal::Missing("pms"))?;
    let url = url.ok_or(Refusal::Missing("url"))?;
    Ok(Trigger { part: parse_part(part)?, pms: parse_pms(pms)?, url: parse_url(url)? })
}

/// A Part id: ASCII digits only (no sign), positive, in range. Zero is refused because the plan
/// records 0 for a Part id it could not read.
fn parse_part(value: &str) -> Result<i64, Refusal> {
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return Err(Refusal::BadPart);
    }
    value.parse::<i64>().ok().filter(|part| *part > 0).ok_or(Refusal::BadPart)
}

/// The server's origin as the harness writes it: `<dotted quad>:<port>`, nothing else. The origin
/// is plaintext HTTP (`Origin::http`): that is what a dev-token boot registers its primary as, and
/// what the request's origin is compared with. The quad is read by `Ipv4Addr`, which refuses a
/// leading zero, so the one spelling the client holds is the one accepted.
fn parse_pms(value: &str) -> Result<Origin, Refusal> {
    let (host, port) = value.split_once(':').ok_or(Refusal::BadPms)?;
    let quad = host.parse::<std::net::Ipv4Addr>().map_err(|_| Refusal::BadPms)?;
    let port_ok = !port.is_empty() && port.len() <= 5 && port.bytes().all(|b| b.is_ascii_digit());
    let port = port.parse::<u32>().ok().filter(|n| port_ok && (1..=65_535).contains(n))
        .and_then(|n| i32::try_from(n).ok()).ok_or(Refusal::BadPms)?;
    Ok(Origin::http(&quad.to_string(), port))
}

/// `http://<dotted quad>:<port>/<path>` over a closed alphabet, with no query or fragment.
fn parse_url(value: &str) -> Result<String, Refusal> {
    if value.contains(['?', '#']) {
        return Err(Refusal::QueryOrFragment);
    }
    if !value.bytes().all(|b| b.is_ascii_alphanumeric()
        || matches!(b, b'.' | b':' | b'/' | b'_' | b'~' | b'%' | b'+' | b'-')) {
        return Err(Refusal::BadCharacter);
    }
    let rest = value.strip_prefix("http://").ok_or(Refusal::NotHttp)?;
    let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
    let (host, port) = authority.split_once(':').ok_or(Refusal::NoPort)?;
    let octets: Vec<&str> = host.split('.').collect();
    let quad = octets.len() == 4 && octets.iter().all(|octet| {
        (1..=3).contains(&octet.len()) && octet.bytes().all(|b| b.is_ascii_digit())
            && octet.parse::<u16>().is_ok_and(|n| n <= 255)
    });
    if !quad {
        return Err(Refusal::HostNotDottedQuad);
    }
    let port_ok = !port.is_empty() && port.len() <= 5 && port.bytes().all(|b| b.is_ascii_digit())
        && port.parse::<u32>().is_ok_and(|n| (1..=65_535).contains(&n));
    if !port_ok {
        return Err(Refusal::BadPort);
    }
    if path.is_empty() {
        return Err(Refusal::NoPath);
    }
    Ok(value.to_owned())
}

/// Decide what `trigger` does on a display bound as `binding`. The trigger's own faults come
/// first; only a well-formed trigger is then held against the display.
pub(crate) fn decide(binding: Binding, trigger: Option<&str>) -> Decision {
    let Some(text) = trigger else { return Decision::Unarmed };
    match parse(text) {
        Err(why) => Decision::Ignored(why),
        Ok(_) if binding != Binding::Acb => Decision::Ignored(Refusal::NotAcbDisplay),
        Ok(trigger) => Decision::Armed(trigger),
    }
}

/// The dev route's composition. With no trigger, a trigger that does not parse, or a display that
/// is not ACB-bound, `evidence` and `qualifier` come back untouched (and the reason is logged
/// unless there was no trigger). Armed on an ACB display they come back as dev evidence and the
/// dev qualifier.
pub(crate) fn compose(evidence: ProductEvidence, qualifier: Arc<dyn OriginalQualifier>,
    trigger: Option<String>) -> (ProductEvidence, Arc<dyn OriginalQualifier>) {
    let decision = decide(evidence.binding, trigger.as_deref());
    if let Some(line) = decision.line() {
        plx_base::eventlog::log(&line);
    }
    match decision {
        Decision::Armed(trigger) => (dev_evidence(evidence), Arc::new(DevQualifier { trigger })),
        Decision::Unarmed | Decision::Ignored(_) => (evidence, qualifier),
    }
}

/// `base` with the four capabilities a URL carrier needs certified; the display path, what is
/// callable and the preview rectangle are the composition's own.
fn dev_evidence(base: ProductEvidence) -> ProductEvidence {
    ProductEvidence {
        certified: Capabilities { progressive_uri: true, hls_uri: false, packet_feed: false,
            pause: true, seek: true, audio_track: true, subtitle_track: false,
            audio_off: false, subtitle_off: false },
        ..base
    }
}

/// The dev qualifier: a plain value, certified for the one Part on the one server.
struct DevQualifier {
    trigger: Trigger,
}

impl OriginalQualifier for DevQualifier {
    fn qualify(&self, _off: &plx_base::task::OffFrame, request: OriginalRequest) -> Verdict {
        if request.selection.part_id != self.trigger.part || request.server_origin != self.trigger.pms {
            return Verdict::Unknown(UriDecline::NotCertified);
        }
        Verdict::Known(KnownSource::url_carrier(&request, self.trigger.url.clone(), VERDICT_TTL))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plx_play_uri::backend::NativeTrackIndex;
    use plx_playback_contract::{Fidelity, SourceKind, TrackChoice, TrackKind, TrackSelection};
    use plx_media::route::uri_preflight::{ProductQualifier, ResolvePort, StandardDirectPlay};
    use plx_media::route::SourceSelection;
    use plx_platform::tv::sink::VP_ACB;

    const GOOD: &str = "acb part=991001 pms=192.0.2.5:32499 \
        url=http://10.1.2.3:8080/pipe_hls_720p24_20m/index.m3u8";
    const URL: &str = "http://10.1.2.3:8080/pipe_hls_720p24_20m/index.m3u8";

    /// The origin `GOOD` pins: what a dev-token boot's client holds for `PMS_HOST:PMS_PORT`.
    fn mock_origin() -> Origin {
        Origin::http("192.0.2.5", 32499)
    }

    fn parsed() -> Trigger {
        parse(GOOD).unwrap_or_else(|why| panic!("the harness's trigger refused: {}", why.reason()))
    }

    // ---- the grammar ----

    #[test]
    fn the_harness_order_parses() {
        let t = parsed();
        assert_eq!((t.part, t.url.as_str()), (991001, URL));
        assert_eq!(t.pms, mock_origin());
    }

    #[test]
    fn the_three_keys_parse_in_any_order() {
        let words = ["part=991001".to_owned(), "pms=192.0.2.5:32499".to_owned(), format!("url={URL}")];
        for order in [[0, 1, 2], [0, 2, 1], [1, 0, 2], [1, 2, 0], [2, 0, 1], [2, 1, 0]] {
            let text = format!("acb {} {} {}", words[order[0]], words[order[1]], words[order[2]]);
            let t = parse(&text).unwrap_or_else(|why| panic!("{text}: {}", why.reason()));
            assert_eq!((t.part, t.pms, t.url.as_str()), (991001, mock_origin(), URL), "{text}");
        }
    }

    #[test]
    fn surrounding_whitespace_and_runs_of_it_are_fine() {
        assert!(parse(&format!("  acb   part=7\tpms=10.0.0.1:80 \n url={URL}\n")).is_ok());
    }

    #[test]
    fn each_refusal_names_its_reason() {
        let cases: &[(&str, Refusal)] = &[
            ("", Refusal::NotAcb),
            ("feed part=1 pms=1.2.3.4:5 url=http://1.2.3.4:80/x", Refusal::NotAcb),
            ("part=1 pms=1.2.3.4:5 url=http://1.2.3.4:80/x", Refusal::NotAcb),
            ("acb pms=1.2.3.4:5 url=http://1.2.3.4:80/x", Refusal::Missing("part")),
            ("acb part=1 url=http://1.2.3.4:80/x", Refusal::Missing("pms")),
            ("acb part=1 pms=1.2.3.4:5", Refusal::Missing("url")),
            ("acb part=1 pms=1.2.3.4:5 url=http://1.2.3.4:80/x ttl=5", Refusal::UnknownKey),
            // the machine-identifier grammar this one replaced is refused loudly, not half-read
            ("acb part=1 server=mock-machine url=http://1.2.3.4:80/x", Refusal::UnknownKey),
            ("acb part=1 pms=1.2.3.4:5 server=m url=http://1.2.3.4:80/x", Refusal::UnknownKey),
            ("acb part=1 part=2 pms=1.2.3.4:5 url=http://1.2.3.4:80/x", Refusal::Repeated),
            ("acb part=1 pms=1.2.3.4:5 pms=1.2.3.4:6 url=http://1.2.3.4:80/x", Refusal::Repeated),
            ("acb part=1 pms=1.2.3.4:5 url=http://1.2.3.4:80/a b", Refusal::NotKeyValue),
            ("acb part=1 pms=1.2.3.4:5 url", Refusal::NotKeyValue),
            ("acb part=abc pms=1.2.3.4:5 url=http://1.2.3.4:80/x", Refusal::BadPart),
            ("acb part= pms=1.2.3.4:5 url=http://1.2.3.4:80/x", Refusal::BadPart),
            ("acb part=0 pms=1.2.3.4:5 url=http://1.2.3.4:80/x", Refusal::BadPart),
            ("acb part=-3 pms=1.2.3.4:5 url=http://1.2.3.4:80/x", Refusal::BadPart),
            ("acb part=+3 pms=1.2.3.4:5 url=http://1.2.3.4:80/x", Refusal::BadPart),
            ("acb part=99999999999999999999 pms=1.2.3.4:5 url=http://1.2.3.4:80/x", Refusal::BadPart),
            ("acb part=1 pms= url=http://1.2.3.4:80/x", Refusal::BadPms),
            ("acb part=1 pms=mock-machine url=http://1.2.3.4:80/x", Refusal::BadPms),
            ("acb part=1 pms=1.2.3.4 url=http://1.2.3.4:80/x", Refusal::BadPms),
            ("acb part=1 pms=1.2.3.4: url=http://1.2.3.4:80/x", Refusal::BadPms),
            ("acb part=1 pms=:5 url=http://1.2.3.4:80/x", Refusal::BadPms),
            ("acb part=1 pms=localhost:5 url=http://1.2.3.4:80/x", Refusal::BadPms),
            ("acb part=1 pms=1.2.3:5 url=http://1.2.3.4:80/x", Refusal::BadPms),
            ("acb part=1 pms=1.2.3.256:5 url=http://1.2.3.4:80/x", Refusal::BadPms),
            ("acb part=1 pms=1.2.3.4.5:5 url=http://1.2.3.4:80/x", Refusal::BadPms),
            ("acb part=1 pms=01.2.3.4:5 url=http://1.2.3.4:80/x", Refusal::BadPms),
            ("acb part=1 pms=1.2.3.4:0 url=http://1.2.3.4:80/x", Refusal::BadPms),
            ("acb part=1 pms=1.2.3.4:65536 url=http://1.2.3.4:80/x", Refusal::BadPms),
            ("acb part=1 pms=1.2.3.4:+5 url=http://1.2.3.4:80/x", Refusal::BadPms),
            ("acb part=1 pms=1.2.3.4:http url=http://1.2.3.4:80/x", Refusal::BadPms),
            ("acb part=1 pms=1.2.3.4:5/x url=http://1.2.3.4:80/x", Refusal::BadPms),
            ("acb part=1 pms=http://1.2.3.4:5 url=http://1.2.3.4:80/x", Refusal::BadPms),
            ("acb part=1 pms=[::1]:5 url=http://1.2.3.4:80/x", Refusal::BadPms),
            ("acb part=1 pms=1.2.3.4:5 url=https://1.2.3.4:80/x", Refusal::NotHttp),
            ("acb part=1 pms=1.2.3.4:5 url=1.2.3.4:80/x", Refusal::NotHttp),
            ("acb part=1 pms=1.2.3.4:5 url=http://localhost:80/x", Refusal::HostNotDottedQuad),
            ("acb part=1 pms=1.2.3.4:5 url=http://1.2.3:80/x", Refusal::HostNotDottedQuad),
            ("acb part=1 pms=1.2.3.4:5 url=http://1.2.3.256:80/x", Refusal::HostNotDottedQuad),
            ("acb part=1 pms=1.2.3.4:5 url=http://1.2.3.4.5:80/x", Refusal::HostNotDottedQuad),
            ("acb part=1 pms=1.2.3.4:5 url=http://1.2.3.4/x", Refusal::NoPort),
            ("acb part=1 pms=1.2.3.4:5 url=http://1.2.3.4:0/x", Refusal::BadPort),
            ("acb part=1 pms=1.2.3.4:5 url=http://1.2.3.4:65536/x", Refusal::BadPort),
            ("acb part=1 pms=1.2.3.4:5 url=http://1.2.3.4:http/x", Refusal::BadPort),
            ("acb part=1 pms=1.2.3.4:5 url=http://1.2.3.4:80", Refusal::NoPath),
            ("acb part=1 pms=1.2.3.4:5 url=http://1.2.3.4:80/", Refusal::NoPath),
            ("acb part=1 pms=1.2.3.4:5 url=http://1.2.3.4:80/x?token=1", Refusal::QueryOrFragment),
            ("acb part=1 pms=1.2.3.4:5 url=http://1.2.3.4:80/x#frag", Refusal::QueryOrFragment),
            ("acb part=1 pms=1.2.3.4:5 url=http://1.2.3.4:80/it's", Refusal::BadCharacter),
            ("acb part=1 pms=1.2.3.4:5 url=http://1.2.3.4:80/a\"b", Refusal::BadCharacter),
            ("acb part=1 pms=1.2.3.4:5 url=http://user@1.2.3.4:80/x", Refusal::BadCharacter),
        ];
        for (text, want) in cases {
            assert_eq!(parse(text).err(), Some(*want), "{text:?}");
        }
    }

    #[test]
    fn the_pms_pin_parses_to_a_plaintext_origin() {
        let t = parse("acb part=1 pms=10.9.8.7:32499 url=http://1.2.3.4:80/x").unwrap();
        assert_eq!(t.pms, Origin::http("10.9.8.7", 32499));
        assert_eq!(t.pms.base(), "http://10.9.8.7:32499");
        for port in ["1", "65535", "32499"] {
            assert!(parse(&format!("acb part=1 pms=10.9.8.7:{port} url={URL}")).is_ok(), "{port}");
        }
    }

    #[test]
    fn each_pms_refusal_reads_the_way_the_log_and_the_harness_expect() {
        assert_eq!(Refusal::Missing("pms").reason(), "pms is missing");
        assert_eq!(Refusal::BadPms.reason(), "pms is not a dotted-quad:port");
        for refusal in [Refusal::Missing("pms"), Refusal::BadPms] {
            let line = Decision::Ignored(refusal).line().unwrap();
            assert!(line.starts_with("uri-dev: ") && line.contains(" ignored"), "{line}");
        }
    }

    #[test]
    fn a_reason_never_carries_the_triggers_own_content() {
        for text in ["acb part=1 pms=10.8.8.8:4242 url=http://10.9.9.9:80/x?t=1",
            "acb part=1 pms=10.8.8.8:4242 url=http://10.9.9.9:80/x ttl=5",
            "acb part=1 pms=10.8.8.8:99999 url=http://10.9.9.9:80/x",
            "acb part=1 pms=10.8.8.8.1:4242 url=http://10.9.9.9:80/x"] {
            let why = parse(text).err().expect("refused").reason();
            assert!(!why.contains("10.8.8.8") && !why.contains("10.9.9.9") && !why.contains("4242"),
                "{why}");
        }
        // nor does the line a refusal logs
        let line = decide(Binding::Acb, Some("acb part=1 pms=10.8.8.8:99999 url=http://10.9.9.9:80/x"))
            .line().unwrap();
        assert!(!line.contains("10.8.8.8") && !line.contains("10.9.9.9"), "{line}");
    }

    // ---- the decision, and the lines it owes ----

    #[test]
    fn no_trigger_is_unarmed_and_silent() {
        let decision = decide(Binding::Acb, None);
        assert!(decision == Decision::Unarmed);
        assert_eq!(decision.line(), None);
    }

    #[test]
    fn an_acb_display_with_a_good_trigger_arms_and_says_which_part() {
        let decision = decide(Binding::Acb, Some(GOOD));
        assert!(matches!(decision, Decision::Armed(_)));
        assert_eq!(decision.line().as_deref(), Some("uri-dev: armed part=991001"));
    }

    #[test]
    fn a_display_that_is_not_acb_ignores_the_trigger_and_says_so() {
        for binding in [Binding::Exported { canvas: (1920, 1080) }, Binding::Unbound] {
            let decision = decide(binding, Some(GOOD));
            assert!(decision == Decision::Ignored(Refusal::NotAcbDisplay));
            assert_eq!(decision.line().as_deref(),
                Some("uri-dev: trigger ignored (display is not ACB)"));
        }
    }

    #[test]
    fn every_refused_trigger_logs_a_line_the_harness_keys_on() {
        for text in ["", "acb", "acb part=1 pms=1.2.3.4:5 url=http://h/x", "acb part=1 pms=h:5 url=x", "bogus"] {
            let line = decide(Binding::Acb, Some(text)).line().expect("a refusal is never silent");
            assert!(line.starts_with("uri-dev: ") && line.contains(" ignored"), "{line}");
            // the harness's regex `uri-dev: .* ignored`
            let at = line.find("uri-dev: ").unwrap() + "uri-dev: ".len();
            assert!(line[at..].contains(" ignored"), "{line}");
        }
    }

    // ---- compose ----

    fn today(binding: Binding) -> ProductEvidence {
        ProductEvidence::unqualified_on(VP_ACB, binding)
    }

    fn certified_names(c: Capabilities) -> Vec<&'static str> {
        [("progressive_uri", c.progressive_uri), ("hls_uri", c.hls_uri),
         ("packet_feed", c.packet_feed), ("pause", c.pause), ("seek", c.seek),
         ("audio_track", c.audio_track), ("subtitle_track", c.subtitle_track),
         ("audio_off", c.audio_off), ("subtitle_off", c.subtitle_off)]
            .into_iter().filter(|(_, on)| *on).map(|(name, _)| name).collect()
    }

    fn same_evidence(a: &ProductEvidence, b: &ProductEvidence) -> bool {
        a.display_exported == b.display_exported && a.binding == b.binding
            && a.callable == b.callable && a.certified == b.certified
            && a.preview_rectangle == b.preview_rectangle
    }

    /// Whatever `compose` hands back for an unarmed boot is the very pair it was given.
    fn assert_untouched(binding: Binding, trigger: Option<&str>) {
        let before = today(binding);
        let qualifier: Arc<dyn OriginalQualifier> = Arc::new(ProductQualifier);
        let (evidence, after) = compose(before, qualifier.clone(), trigger.map(str::to_owned));
        assert!(same_evidence(&evidence, &before), "{binding:?} {trigger:?}");
        assert!(Arc::ptr_eq(&qualifier, &after), "{binding:?} {trigger:?}: the qualifier changed");
        assert!(certified_names(evidence.certified).is_empty(), "production certifies nothing");
    }

    #[test]
    fn without_the_trigger_compose_returns_todays_pair() {
        assert_untouched(Binding::Acb, None);
    }

    #[test]
    fn without_acb_compose_returns_todays_pair() {
        assert_untouched(Binding::Exported { canvas: (1920, 1080) }, Some(GOOD));
        assert_untouched(Binding::Unbound, Some(GOOD));
    }

    #[test]
    fn a_trigger_that_does_not_parse_leaves_todays_pair() {
        assert_untouched(Binding::Acb, Some("acb part=1"));
        assert_untouched(Binding::Acb, Some(""));
    }

    #[test]
    fn armed_on_acb_certifies_exactly_the_url_carrier_capabilities_and_swaps_the_qualifier() {
        let before = today(Binding::Acb);
        let qualifier: Arc<dyn OriginalQualifier> = Arc::new(ProductQualifier);
        let (evidence, after) = compose(before, qualifier.clone(), Some(GOOD.to_owned()));
        assert_eq!(evidence.binding, Binding::Acb, "the binding is taken, never forced");
        assert_eq!(certified_names(evidence.certified),
            ["progressive_uri", "pause", "seek", "audio_track"]);
        assert!(evidence.callable == before.callable && evidence.display_exported
            == before.display_exported && evidence.preview_rectangle == before.preview_rectangle);
        assert!(!Arc::ptr_eq(&qualifier, &after));
    }

    // ---- the dev qualifier ----

    fn request_at(part: i64, server: &Origin, audio_sid: i64, audio_ordinal: Option<i64>) -> OriginalRequest {
        OriginalRequest {
            selection: SourceSelection { server: plx_plex::plex::ServerId::from_raw(1),
                part_id: part, audio_sid, subtitle_sid: 0,
                audio_contract: plx_plex::plex::AudioEnhancements::NONE },
            server_origin: server.clone(),
            source_url: "http://pms.invalid/library/parts/9/file.mkv".to_owned(),
            duration_ms: Some(90_000),
            video_codec: "h264".to_owned(),
            dolby_vision: plx_data::metadata::Dovi::NONE,
            dolby_vision_mode: plx_plex::plex::session::DolbyVisionMode::Auto, uri_opt_in: false,
            standard: StandardDirectPlay { video: true, dolby_vision: true, audio: true,
                container: true },
            audio_codec: "aac".to_owned(), audio_ordinal, has_multiple_audio_tracks: false, binding: Binding::Unbound,
            subtitle_source: None,
            subtitle_choice: TrackChoice::Unspecified,
            source_kbps: Some(4000), raster: Some((1280, 720)),
            requested_rate_permille: plx_playback_contract::RATE_NORMAL, resume_ns: 0,
            quality_allows_original: true, link_allows_original: true, auto_allows_original: true,
            requires_subtitle_burn: false, requires_client_subtitle: false, preview: false,
        }
    }

    /// A request for a Part whose selected audio stream `audio_sid` is the Part's first audio stream.
    fn request(part: i64, server: &Origin, audio_sid: i64) -> OriginalRequest {
        request_at(part, server, audio_sid, (audio_sid > 0).then_some(0))
    }

    fn armed_pair() -> (ProductEvidence, Arc<dyn OriginalQualifier>) {
        let (evidence, qualifier) = compose(today(Binding::Acb), Arc::new(ProductQualifier),
            Some(GOOD.to_owned()));
        assert!(certified_names(evidence.certified).contains(&"progressive_uri"), "armed");
        (evidence, qualifier)
    }

    #[test]
    fn the_qualifier_certifies_the_triggers_url_for_its_part_on_its_origin() {
        let (_, qualifier) = armed_pair();
        let off = plx_base::task::OffFrame::for_test();
        let Verdict::Known(known) = qualifier.qualify(&off, request(991001, &mock_origin(), 42))
            else { panic!("the pinned part on the pinned server was not certified") };
        assert_eq!(known.selection.part_id, 991001);
        assert_eq!(known.requirements.source, SourceKind::ProgressiveUri);
        assert_eq!((known.requirements.video, known.requirements.audio),
            (Fidelity::Exact, Fidelity::Exact));
        assert_eq!(known.requirements.audio_track, TrackChoice::Selected(42));
        assert!(known.source.can_supply(known.requirements), "the selected audio has a native index");
        assert!(known.expires_at > std::time::Instant::now());
        assert!(known.certified_dolby_vision.is_none() && known.certified_subtitle_source.is_none());
        let (url, tracks) = known.source.into_parts();
        assert_eq!(url, URL, "the trigger's URL, not the PMS Part's");
        assert_eq!(tracks, [NativeTrackIndex { selection: TrackSelection {
            kind: TrackKind::Audio, id: 42 }, index: 0 }]);
    }

    /// **W6.** The selected audio stream maps to ITS position among the Part's audio streams, not to a
    /// fixed 0: the carrier lists its audio in the Part's order, and the service loads on track 0, so a
    /// second stream is only reached by a `selectTrack` to native index 1. RED while the qualifier
    /// mapped every selected stream to index 0 (the backend then found the service already there and
    /// sent nothing).
    #[test]
    fn the_selected_audio_stream_maps_to_its_position_among_the_parts_audio_streams() {
        let (_, qualifier) = armed_pair();
        let off = plx_base::task::OffFrame::for_test();
        for (sid, ordinal) in [(99100101, 0), (99100102, 1), (99100103, 2)] {
            let Verdict::Known(known) = qualifier.qualify(&off,
                request_at(991001, &mock_origin(), sid, Some(ordinal))) else { panic!("not certified") };
            assert!(known.source.can_supply(known.requirements), "sid {sid}");
            assert_eq!(known.source.into_parts().1, [NativeTrackIndex { selection: TrackSelection {
                kind: TrackKind::Audio, id: sid as u64 }, index: ordinal }], "sid {sid}");
        }
    }

    /// A selected stream whose position the plan could not name has no native index: the source cannot
    /// supply it (the route then plays the Feed) rather than quietly selecting track 0.
    #[test]
    fn a_selected_audio_stream_with_no_known_position_maps_nothing_and_cannot_be_supplied() {
        let (_, qualifier) = armed_pair();
        let off = plx_base::task::OffFrame::for_test();
        let Verdict::Known(known) = qualifier.qualify(&off,
            request_at(991001, &mock_origin(), 42, None)) else { panic!("not certified") };
        assert_eq!(known.requirements.audio_track, TrackChoice::Selected(42));
        assert!(!known.source.can_supply(known.requirements));
    }

    #[test]
    fn with_no_audio_selected_the_source_maps_no_audio_track() {
        let (_, qualifier) = armed_pair();
        let off = plx_base::task::OffFrame::for_test();
        let Verdict::Known(known) = qualifier.qualify(&off, request(991001, &mock_origin(), 0))
            else { panic!("not certified") };
        assert_eq!(known.requirements.audio_track, TrackChoice::Unspecified);
        assert!(known.source.into_parts().1.is_empty());
    }

    #[test]
    fn another_part_is_unknown() {
        let (_, qualifier) = armed_pair();
        let off = plx_base::task::OffFrame::for_test();
        for part in [991002, 1, 0, -991001] {
            assert!(matches!(qualifier.qualify(&off, request(part, &mock_origin(), 42)),
                Verdict::Unknown(UriDecline::NotCertified)), "part {part}");
        }
    }

    #[test]
    fn another_origin_is_unknown() {
        let (_, qualifier) = armed_pair();
        let off = plx_base::task::OffFrame::for_test();
        for other in [Origin::http("192.0.2.6", 32499), Origin::http("192.0.2.5", 32498),
            Origin::http("192.0.2.5", 32400), Origin::http("127.0.0.1", 32499),
            Origin::new(plx_plex::plex::Scheme::Https, "192.0.2.5", 32499)] {
            assert!(matches!(qualifier.qualify(&off, request(991001, &other, 42)),
                Verdict::Unknown(UriDecline::NotCertified)), "origin {}", other.base());
        }
    }

    /// A test run forces the trigger's URL for its Part whatever Standard would do with the file (the
    /// production opt-in answers only for what Standard direct-plays): the dev qualifier itself reads none
    /// of the plan's verdicts, and `url_carrier` reads only the Dolby Vision one (it certifies a DV record
    /// only where `standard.dolby_vision`), so a fixture Standard converts for its video, audio or
    /// container still plays here, while a Dolby Vision record the frozen decision refused is not forced.
    #[test]
    fn the_dev_qualifier_forces_the_url_whatever_standard_would_do_with_the_file() {
        let (_, qualifier) = armed_pair();
        let off = plx_base::task::OffFrame::for_test();
        let mut req = request(991001, &mock_origin(), 42);
        req.standard = StandardDirectPlay { video: false, dolby_vision: false, audio: false,
            container: false };
        let Verdict::Known(known) = qualifier.qualify(&off, req)
            else { panic!("the dev route did not force the pinned part") };
        assert_eq!(known.source.into_parts().0, URL);
    }

    #[test]
    fn a_dolby_vision_request_is_certified_for_exactly_that_layering() {
        let (_, qualifier) = armed_pair();
        let off = plx_base::task::OffFrame::for_test();
        let mut req = request(991001, &mock_origin(), 0);
        req.dolby_vision = plx_data::metadata::Dovi { present: true, profile: 8,
            ..plx_data::metadata::Dovi::NONE };
        let dovi = req.dolby_vision;
        let Verdict::Known(known) = qualifier.qualify(&off, req) else { panic!("not certified") };
        assert_eq!(known.certified_dolby_vision, Some(dovi));
    }

    #[test]
    fn the_armed_port_passes_the_resolve_ports_own_checks_and_the_unarmed_port_does_not() {
        let off = plx_base::task::OffFrame::for_test();
        let (evidence, qualifier) = armed_pair();
        let armed = ResolvePort::new(evidence, qualifier);
        assert!(matches!(armed.qualify(&off, request(991001, &mock_origin(), 42)),
            Verdict::Known(_)));
        assert!(matches!(armed.qualify(&off, request(991001, &Origin::http("192.0.2.6", 32499), 42)),
            Verdict::Unknown(UriDecline::NotCertified)));
        let unarmed = ResolvePort::new(today(Binding::Acb), Arc::new(ProductQualifier));
        assert!(matches!(unarmed.qualify(&off, request(991001, &mock_origin(), 42)),
            Verdict::Unknown(UriDecline::NotOptedIn)), "the production qualifier answers only an opt-in");
    }
}
