//! **Open a title another app asked for** — a launcher's Continue Watching row, say.
//!
//! The contract, as a caller writes it (`docs/launch-links.md` is the caller's copy):
//!
//! ```text
//! luna://com.webos.applicationManager/launch
//! {"id":"com.beb.plxnative","params":{"plex":{"server":"<machineIdentifier>","ratingKey":"12345","action":"open"}}}
//! ```
//!
//! - `ratingKey` (required): the item to open, exactly as given. A string or a number, digits only.
//! - `server` (optional): the server's `machineIdentifier`. Without it, the signed-in current server.
//! - `action` (optional): `open` (the default) lands on the item's detail page; `play` starts it
//!   where Resume would. Anything else is `open`.
//! - Unknown keys are ignored. No `plex` object at all is an ordinary launch.
//! - A launch while the app runs carries the link too, where LG's SDL lets the app read relaunch
//!   payloads (`app::boot::tap_relaunch_payloads`, webOS 7.4 on); the page then replaces wherever
//!   the app was. Elsewhere (webOS 4.x to 6.x) only a cold launch does, and a caller closes the app first
//!   (`com.webos.applicationManager/closeByAppId`), then launches.
//!
//! The payload arrives raw through `plx_platform::tv::launch` (`argv[1]`, or a relaunch event). This
//! module waits until
//! there is a signed-in profile with Home at the root, makes sure Home is alone on the stack, asks
//! for the item's metadata and hands the landing to the shared menu-play wait
//! (`input::MenuPlayAwait::for_link`), which opens or plays it; a key the server does not know
//! leaves Home in place (and a line in the event log), never a blank page. Sign-in,
//! the profile picker and onboarding are never interrupted: the link waits behind the picker and
//! onboarding, and is dropped when no account is signed in.
use plx_screens::registry::AppArg;

/// What the launch asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Action { Open, Play }

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Link {
    pub(crate) server: Option<String>,
    pub(crate) rk: String,
    pub(crate) action: Action,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Parsed {
    /// No `plex` object: an ordinary launch.
    None,
    /// A `plex` object that names nothing openable.
    Bad,
    Link(Link),
}

/// Read a launch payload: SAM puts the caller's params under `parameters` in `argv[1]`.
pub(crate) fn parse(raw: &str) -> Parsed {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(raw) else { return Parsed::None };
    let Some(plex) = v.get("parameters").and_then(|p| p.get("plex")) else {
        return Parsed::None;
    };
    let rk = match plex.get("ratingKey") {
        Some(serde_json::Value::String(s)) => s.trim().to_owned(),
        Some(serde_json::Value::Number(n)) if n.is_u64() => n.to_string(),
        _ => return Parsed::Bad,
    };
    if rk.is_empty() || rk.len() > 20 || !rk.bytes().all(|b| b.is_ascii_digit()) {
        return Parsed::Bad;
    }
    let server = plex.get("server").and_then(|s| s.as_str()).map(str::trim)
        .filter(|s| !s.is_empty()).map(str::to_owned);
    let action = match plex.get("action").and_then(|a| a.as_str()) {
        Some("play") => Action::Play,
        _ => Action::Open,
    };
    Parsed::Link(Link { server, rk, action })
}

/// How long a link waits for its server to appear in the roster once Home is up: shared servers
/// are discovered after Home mounts. The metadata wait after it is `MenuPlayAwait`'s own ceiling.
const SERVER_WAIT_MS: u32 = 15_000;

/// The link not yet handed to the menu-play wait. Owned by `App`, driven by [`tick`] every frame,
/// which also takes each payload that arrives.
#[derive(Default)]
pub(crate) enum DeepLink {
    #[default]
    Idle,
    /// Parsed, waiting for a session with Home at the root. `server_deadline` is set the first
    /// frame Home is the root and bounds the wait for the link's server.
    Waiting { link: Link, server_deadline: Option<u32> },
}

impl DeepLink {
    /// The link a launch carried in `raw`, if any.
    pub(crate) fn from_launch(raw: Option<String>) -> DeepLink {
        match raw.as_deref().map(parse) {
            None | Some(Parsed::None) => DeepLink::Idle,
            Some(Parsed::Bad) => {
                not_found("bad ratingKey");
                DeepLink::Idle
            }
            Some(Parsed::Link(link)) => {
                plx_base::eventlog::log(&format!(
                    "deeplink: {:?} rk={} server={}", link.action, link.rk,
                    if link.server.is_some() { "named" } else { "current" }));
                DeepLink::Waiting { link, server_deadline: None }
            }
        }
    }
}

/// Where a waiting link stands, from the stack's ROOT page. Pure, so the gate is tested without a
/// session.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Gate {
    /// Nothing to act in yet: boot, the profile picker, onboarding, a consent question.
    Wait,
    /// No account is signed in: the link is dropped.
    Drop,
    /// Home is the root: the app is signed in and settled.
    Home,
}

pub(crate) fn gate(root: Option<&AppArg>) -> Gate {
    match root {
        Some(AppArg::Login) => Gate::Drop,
        Some(AppArg::Home) => Gate::Home,
        _ => Gate::Wait,
    }
}

fn expired(now: u32, deadline: u32) -> bool { now.wrapping_sub(deadline) < u32::MAX / 2 }

fn resolve(server: Option<&str>) -> Option<plx_plex::plex::ServerId> {
    use plx_plex::plex as servers;
    match server {
        Some(machine) => servers::id_of_machine(machine),
        None => servers::client_opt().map(|_| servers::current_server()),
    }
}

/// The link went nowhere: Home stays as it is. Only the log says so; a launcher passes keys it
/// read from the same server, so a miss is an edge case not worth a message on screen.
fn not_found(why: &str) {
    plx_base::eventlog::log(&format!("deeplink: not opened ({why})"));
}

/// One frame of the link: take a newly arrived payload, then, once Home is the root, alone, and
/// the link's server is known, ask for the item and arm the menu-play wait that lands it. Runs
/// just before `menu_play_tick`.
pub(crate) fn tick(
    pages: &mut plx_ui::dispatch::Dispatcher<super::bridge::AppHost>,
    bridge: &mut super::bridge::Bridge,
    state: &mut DeepLink,
    menu_play_await: &mut Option<super::input::MenuPlayAwait>,
    now: u32,
) {
    if let Some(raw) = plx_platform::tv::launch::take() {
        // A launch without a link (the launcher's own icon) leaves one still waiting alone.
        let next = DeepLink::from_launch(Some(raw));
        if matches!(next, DeepLink::Waiting { .. }) {
            *state = next;
        }
    }
    let DeepLink::Waiting { link, server_deadline } = state else { return };
    match gate(pages.nav.tabs.stack.entries.first().map(|e| &e.arg)) {
        Gate::Wait => {
            *server_deadline = None;
            return;
        }
        Gate::Drop => {
            plx_base::eventlog::log("deeplink: no account signed in - dropped");
            *state = DeepLink::Idle;
            return;
        }
        Gate::Home => {}
    }
    let deadline = *server_deadline.get_or_insert(now.wrapping_add(SERVER_WAIT_MS));
    let Some(sid) = resolve(link.server.as_deref()) else {
        if expired(now, deadline) {
            not_found("server not available");
            *state = DeepLink::Idle;
        }
        return;
    };
    // Home alone underneath, so BACK off the page lands there: unwind whatever stands above it
    // and wait for that to settle before asking.
    if !super::bridge::top_settled_on(pages, &AppArg::Home) {
        if !pages.has_pending_navigation() {
            super::bridge::nav_select_tab(pages, AppArg::Home);
        }
        return;
    }
    let DeepLink::Waiting { link, .. } = std::mem::take(state) else { return };
    bridge.metadata_mut().run(plx_data::stores::metadata::MetadataCmd::RequestDetail {
        sid, rk: link.rk.clone() });
    *menu_play_await = Some(super::input::MenuPlayAwait::for_link(sid, link.rk, link.action == Action::Open, now));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(server: Option<&str>, rk: &str, action: Action) -> Parsed {
        Parsed::Link(Link { server: server.map(str::to_owned), rk: rk.to_owned(), action })
    }

    /// The cold-launch argument exactly as SAM wrote it on the television (webOS 9.2.2).
    #[test]
    fn reads_sams_cold_launch_argument() {
        let raw = r#"{"event":"launch","reason":"undefined","appId":"com.beb.plxnative","parameters":{"plex":{"server":"abc123","ratingKey":"12345","action":"open"}},"interfaceVersion":2,"interfaceMethod":"registerApp","@system_native_app":true}"#;
        assert_eq!(parse(raw), link(Some("abc123"), "12345", Action::Open));
    }

    #[test]
    fn reads_play() {
        let raw = r#"{"event":"launch","parameters":{"plex":{"ratingKey":"77","action":"play"}}}"#;
        assert_eq!(parse(raw), link(None, "77", Action::Play));
    }

    #[test]
    fn server_is_optional_and_unknown_keys_are_ignored() {
        let p = |plex: &str| parse(&format!(r#"{{"parameters":{{"plex":{plex}}}}}"#));
        assert_eq!(p(r#"{"ratingKey":42,"future":true}"#), link(None, "42", Action::Open));
        assert_eq!(p(r#"{"ratingKey":"9","server":""}"#), link(None, "9", Action::Open));
        assert_eq!(p(r#"{"ratingKey":"9","action":"rewind"}"#), link(None, "9", Action::Open));
        // The params object itself, outside `parameters`, is not where SAM puts it.
        assert_eq!(parse(r#"{"plex":{"ratingKey":"9"}}"#), Parsed::None);
    }

    #[test]
    fn junk_is_bad_and_no_plex_object_is_an_ordinary_launch() {
        assert_eq!(parse("not json"), Parsed::None);
        assert_eq!(parse(r#"{"event":"launch","parameters":{}}"#), Parsed::None);
        assert_eq!(parse(r#"{"event":"launch","parameters":{"other":1}}"#), Parsed::None);
        for bad in [
            r#"{}"#,
            r#""12345""#,
            r#"{"ratingKey":""}"#,
            r#"{"ratingKey":"12a"}"#,
            r#"{"ratingKey":"../library"}"#,
            r#"{"ratingKey":-3}"#,
            r#"{"ratingKey":1.5}"#,
        ] {
            assert_eq!(parse(&format!(r#"{{"parameters":{{"plex":{bad}}}}}"#)), Parsed::Bad, "{bad}");
        }
    }

    /// A Sentry crash envelope path (`main.c` hands that to the spooler first) is not a launch.
    #[test]
    fn a_sentry_envelope_path_is_not_a_link() {
        assert_eq!(parse("/tmp/plxnative/.sentry-native/abc.envelope"), Parsed::None);
    }

    #[test]
    fn the_gate_waits_behind_the_picker_and_drops_without_an_account() {
        assert_eq!(gate(None), Gate::Wait);
        assert_eq!(gate(Some(&AppArg::Profiles)), Gate::Wait);
        assert_eq!(gate(Some(&AppArg::Onboard)), Gate::Wait);
        assert_eq!(gate(Some(&AppArg::Login)), Gate::Drop);
        assert_eq!(gate(Some(&AppArg::Home)), Gate::Home);
    }

    #[test]
    fn deadlines_wrap() {
        assert!(!expired(100, 200));
        assert!(expired(200, 200));
        assert!(expired(5, u32::MAX - 5));
    }
}
