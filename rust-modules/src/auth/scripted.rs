//! The sign-in worker's SCRIPTED outcomes: the two dev triggers that stand in for plex.tv, read
//! beside the worker that consumes them.
//!
//! * `plxnative-readout=<case>` boots the sign-in screen straight into a canned terminal failure
//!   ([`ReadoutCase`]), with no PIN minted and no call made.
//! * `plxnative-signinfail[=…]` makes the real requests fail the way an unresolvable plex.tv
//!   would ([`signin_trouble_create`], [`signin_trouble_poll`], [`signin_trouble_resources`]).
//!
//! Both are for the ui-sim captures of the incident offer. Both produce REAL state through the
//! production seams: the sign-in trouble is evidence handed to the same producers a failed request
//! feeds. Nothing here paints a screen directly.
//!
//! They lived in `dev::scenarios` and were reached UP from `auth`. They only read a trigger file
//! (through `plx_base::devtrig`) and build values of the account, net and telemetry layers, all of
//! which `auth` may name, so the reads moved down here instead of `auth` naming the app-layer
//! `dev` module. With `devtriggers` off, `plx_base::devtrig::read` is `None` at compile time and
//! every function below answers "no script", exactly as before.

/// `/tmp/plxnative-readout=<case>` — boot straight into a chosen page-filling `Failed` read-out
/// for the read-out glyph work's own visual verification (spec "1A"), with NO network call and no
/// account touched: paired with `plxnative-login` (which already forces `BootTo::Login` with no
/// session), `login_worker_with_output` reads this ONCE at the top of the worker thread and, for
/// every case named here, skips straight to `auth::output_failed_naming` with a canned caption,
/// [`plx_telemetry::telemetry::incident::IncidentContext`] and (`discovery_no_servers` only) a canned account name instead of minting a PIN or discovering
/// anything — the exact same terminal path a real failure reaches, so `LoginScreen`'s `Phase::
/// Error` draw, `readout_kind` and `readout_glyph` are exercised UNMODIFIED. Every value below is
/// the one `auth::discovery_failure`'s table would have built for the same cause; see that
/// function and `telemetry::incident::IncidentContext::readout_glyph`'s doc for why each case maps
/// to the glyph it does. `Home`, `Library` and `ProfileSwitch` read-outs are NOT covered by this
/// trigger — they are not reached through the sign-in worker, and short-circuiting their own
/// stores/session state the same way needs plumbing this pass did not reach; verify a glyph in
/// that family through `StatusOverlay`'s host geometry tests and its shared `page()` code path
/// instead (the same function every case here also draws through).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReadoutCase {
    PinCreate,
    PinExpired,
    Authorization,
    DiscoveryNoServers,
    DiscoveryRefused,
    DiscoveryServersSilent,
    DiscoveryPlexTvDns,
    DiscoveryPlexTvTls,
    DiscoveryPlexTvAnswered,
    DiscoveryPlexTvOther,
    DiscoveryInsecureOnly,
    SaveFailed,
}

pub(crate) fn readout_case() -> Option<ReadoutCase> {
    plx_base::devtrig::read("readout").and_then(|s| match s.trim() {
        "pin_create" => Some(ReadoutCase::PinCreate),
        "pin_expired" => Some(ReadoutCase::PinExpired),
        "authorization" => Some(ReadoutCase::Authorization),
        "discovery_no_servers" => Some(ReadoutCase::DiscoveryNoServers),
        "discovery_refused" => Some(ReadoutCase::DiscoveryRefused),
        "discovery_servers_silent" => Some(ReadoutCase::DiscoveryServersSilent),
        "discovery_plextv_dns" => Some(ReadoutCase::DiscoveryPlexTvDns),
        "discovery_plextv_tls" => Some(ReadoutCase::DiscoveryPlexTvTls),
        "discovery_plextv_answered" => Some(ReadoutCase::DiscoveryPlexTvAnswered),
        "discovery_plextv_other" => Some(ReadoutCase::DiscoveryPlexTvOther),
        "discovery_insecure_only" => Some(ReadoutCase::DiscoveryInsecureOnly),
        "save_failed" => Some(ReadoutCase::SaveFailed),
        _ => None,
    })
}

/// The mock account the canned `discovery_no_servers` read-out says it signed in as, unless
/// `/tmp/plxnative-readout-account=<name>` names another (an empty file shows the nameless
/// `browse.auth.no_servers` fallback). A canned name, never a real account's.
const MOCK_ACCOUNT: &str = "alexandra";

impl ReadoutCase {
    /// The account name the canned failure carries to the screen (`LoginProgress::Failed::account`),
    /// the way a real no-server sign-in does: composed and measured there, not here. Only
    /// `discovery_no_servers` has one.
    pub(crate) fn canned_account(self) -> Option<String> {
        if self != Self::DiscoveryNoServers { return None; }
        Some(plx_base::devtrig::read("readout-account").unwrap_or_else(|| MOCK_ACCOUNT.to_string()))
            .filter(|name| !name.trim().is_empty())
    }
    /// The canned caption + [`IncidentContext`](plx_telemetry::telemetry::incident::IncidentContext)
    /// `login_worker_with_output` feeds `output_failed_naming` (with [`Self::canned_account`]) in place of the real network calls — see
    /// [`readout_case`]'s doc.
    pub(crate) fn canned_login_failure(
        self,
    ) -> (std::borrow::Cow<'static, str>, plx_telemetry::telemetry::incident::IncidentContext) {
        use plx_platform::i18n::msg;
        use plx_telemetry::telemetry::incident::{
            DiscoveryClass, DiscoveryEvidence, DiscoveryTarget, DiscoveryTrigger, IncidentContext,
            IncidentKind, LinkClass,
        };
        fn ctx(kind: IncidentKind) -> IncidentContext {
            IncidentContext::new(kind, None)
        }
        fn plextv(target: DiscoveryTarget) -> DiscoveryEvidence {
            DiscoveryEvidence { trigger: DiscoveryTrigger::Login, target: Some(target) }
        }
        match self {
            Self::PinCreate => (msg::browse_auth_plex_unreachable().into(), ctx(IncidentKind::PinCreate)),
            Self::PinExpired => (msg::browse_auth_timeout().into(), ctx(IncidentKind::PinExpired)),
            Self::Authorization => (msg::browse_auth_signin_refused().into(), ctx(IncidentKind::Authorization)),
            Self::DiscoveryNoServers => (
                msg::browse_auth_no_servers().into(),
                ctx(IncidentKind::Discovery(DiscoveryClass::NoServers)),
            ),
            Self::DiscoveryRefused => (
                msg::browse_auth_refused().into(),
                ctx(IncidentKind::Discovery(DiscoveryClass::Refused)),
            ),
            Self::DiscoveryServersSilent => {
                let mut c = ctx(IncidentKind::Discovery(DiscoveryClass::Silent));
                c.discovery = Some(plextv(DiscoveryTarget::Servers));
                (msg::browse_auth_servers_unreachable().into(), c)
            }
            Self::DiscoveryPlexTvDns => {
                let mut c = ctx(IncidentKind::Discovery(DiscoveryClass::Silent));
                c.link = LinkClass::Dns;
                c.discovery = Some(plextv(DiscoveryTarget::PlexTv));
                (msg::browse_auth_plex_dns_retry(1).into(), c)
            }
            Self::DiscoveryPlexTvTls => {
                let mut c = ctx(IncidentKind::Discovery(DiscoveryClass::Silent));
                c.link = LinkClass::Tls;
                c.discovery = Some(plextv(DiscoveryTarget::PlexTv));
                (msg::browse_auth_plex_tls().into(), c)
            }
            Self::DiscoveryPlexTvAnswered => {
                let mut c = ctx(IncidentKind::Discovery(DiscoveryClass::Silent));
                c.link = LinkClass::Answered5xx;
                c.discovery = Some(plextv(DiscoveryTarget::PlexTv));
                (msg::browse_auth_plex_unavailable().into(), c)
            }
            Self::DiscoveryPlexTvOther => {
                let mut c = ctx(IncidentKind::Discovery(DiscoveryClass::Silent));
                c.discovery = Some(plextv(DiscoveryTarget::PlexTv));
                (msg::browse_auth_plex_connect_retry(1).into(), c)
            }
            Self::DiscoveryInsecureOnly => (
                msg::browse_auth_insecure().into(),
                ctx(IncidentKind::Discovery(DiscoveryClass::InsecureOnly)),
            ),
            // The real "Couldn't save your sign-in" warning arrives through a different path
            // (`persistence_warning`, drawn by `LoginScreen::draw_warning`) — this case takes the
            // ordinary `Phase::Error` route instead, which draws a different caption ("Couldn't
            // sign in") but the SAME page-placed `Failed` layout and the SAME `KeyBadgeAlert`
            // glyph, which is the only thing this trigger exists to show.
            Self::SaveFailed => (msg::browse_login_failed().into(), ctx(IncidentKind::SaveFailed)),
        }
    }
}

/// The synthetic failure `plxnative-signinfail` stands for: a name that did not resolve, the
/// commonest way a television "has no internet". A `net::RequestFailure` like the one libcurl's
/// `CURLE_COULDNT_RESOLVE_HOST` produces, planted only into the one call it replaces — never into
/// `net`'s own records, which other callers read as real evidence (0.6.6's reason, kept).
fn synthetic_dns_failure() -> plx_net::net::RequestFailure {
    plx_net::net::RequestFailure {
        cause: plx_net::net::RequestError::Transport,
        status: None,
        body_limit: None,
        curl_rc: Some(6),
    }
}

fn signinfail_spec() -> Option<String> {
    if cfg!(test) { return None; }
    plx_base::devtrig::read("signinfail")
}

/// `/tmp/plxnative-signinfail[=error]` — every sign-in code request fails as an unresolvable
/// plex.tv would. Re-read at each attempt, so *Try again* fails the same way until it is removed.
/// `None` means "make the real request".
pub(crate) fn signin_trouble_create()
    -> Option<Result<plx_plex::plex::account::Pin, plx_plex::plex::account::CallEvidence>> {
    match signinfail_spec()?.as_str() {
        "" | "error" => {
            plx_base::eventlog::log("dev: signinfail — the sign-in code request fails (synthetic DNS failure)");
            Some(Err(Err(synthetic_dns_failure())))
        }
        _ => None,
    }
}

/// `/tmp/plxnative-signinfail=stall` — the code is real, but every poll of it goes unanswered, so
/// the wait reaches the stalled rule (`auth::LINK_TROUBLE_AFTER`) exactly as a dropped link would.
pub(crate) fn signin_trouble_poll() -> Option<plx_plex::plex::account::PinPoll> {
    (signinfail_spec()?.as_str() == "stall")
        .then(|| plx_plex::plex::account::PinPoll::Unreachable(Err(synthetic_dns_failure())))
}

/// `/tmp/plxnative-signinfail=resources|resources-blip` — fail the plex.tv resource listing on
/// every attempt, or on its first attempt only. The latter exercises the in-place retry while the
/// sign-in screen remains in Discovering.
pub(crate) fn signin_trouble_resources()
    -> Option<Result<Vec<plx_plex::plex::account::Resource>, plx_plex::plex::account::CallEvidence>> {
    static BLIPPED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    match signinfail_spec()?.as_str() {
        "resources" => Some(Err(Err(synthetic_dns_failure()))),
        "resources-blip" if !BLIPPED.swap(true, std::sync::atomic::Ordering::AcqRel) =>
            Some(Err(Err(synthetic_dns_failure()))),
        _ => None,
    }
}
