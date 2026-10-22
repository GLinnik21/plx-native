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
//! leaves Home in place (and a line in the event log), never a blank page.
//!
//! **When it acts.** The link is a pure step ([`DeepLink::step`]): one event and the read-only
//! facts of the frame in ([`link_inputs`] is the only reader of the outside world), effect values
//! out ([`LinkOut`], performed by [`perform`], which decides nothing). It waits, and never
//! interrupts, behind sign-in, the profile picker and onboarding (the root is not Home), behind any
//! surface that is up (the first-run consent question), and while the window is backgrounded.
//! A link that names a server survives a sign-in and resolves against the roster of the account
//! that signed in (the wait for the server starts the first frame Home is the root, and ends
//! after `SERVER_WAIT_MS`); a link that names none means "the signed-in server" and, having waited
//! across a sign-in, cannot know which account it was meant for: it is dropped.
use plx_machine::machine::{Fx, LogLine, MachineId, NavOp};
use plx_plex::plex::ServerId;
use plx_screens::registry::{AppArg, AppFx};

use super::bridge::AppHost;

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

/// What one frame brings the link: a payload that arrived (already read by pure [`parse`]), or
/// nothing new.
pub(crate) enum LinkEvent { Arrived(Parsed), Frame }

/// The roster facts [`DeepLink::step`] may ask about: a synchronous read port. Production's
/// [`GlobalRoster`] is the only code that reads the servers registry for the link; tests pass a
/// fixture.
pub(crate) trait LinkRoster {
    /// The server with this `machineIdentifier`, once the roster knows it.
    fn by_machine(&self, machine: &str) -> Option<ServerId>;
    /// The signed-in current server, once there is one.
    fn current(&self) -> Option<ServerId>;
}

/// Where the stack's root stands, as the link reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Root {
    /// Sign-in: no account yet. The link waits, and remembers it waited across a sign-in.
    Login,
    /// Home: signed in and settled.
    Home,
    /// Boot, the profile picker, onboarding: nothing to act in yet.
    Other,
}

/// Read-only facts for one step, built by [`link_inputs`] alone.
pub(crate) struct LinkEnv<'a> {
    pub(crate) now: u32,
    /// The window is foregrounded (`WindowActivity::allow_present`).
    pub(crate) foreground: bool,
    pub(crate) root: Root,
    /// A surface (the consent question, a menu) is up over the stack.
    pub(crate) surface_up: bool,
    pub(crate) nav_pending: bool,
    /// Home is the settled top with nothing above it.
    pub(crate) home_settled: bool,
    pub(crate) roster: &'a dyn LinkRoster,
}

/// What a step asks for: existing effect values, plus the one app-loop write.
#[derive(Default)]
pub(crate) struct LinkOut {
    pub(crate) fx: Vec<Fx<AppHost>>,
    /// The wait that lands the item: `(server, ratingKey, open instead of play)`. `MenuPlayAwait`
    /// is an inline driver with no effect value, so this stays an app-loop write.
    pub(crate) arm: Option<(ServerId, String, bool)>,
}

/// The link not yet handed to the menu-play wait. Owned by `App`; the state is private, so only
/// [`DeepLink::step`] writes it.
#[derive(Default)]
pub(crate) struct DeepLink(LinkState);

#[derive(Default)]
enum LinkState {
    #[default]
    Idle,
    /// Parsed, waiting to act. `server_deadline` is set the first frame Home is the root and
    /// bounds the wait for the link's server; `crossed_sign_in` is set when the root was the
    /// sign-in screen while the link waited.
    Waiting { link: Link, server_deadline: Option<u32>, crossed_sign_in: bool },
}

fn expired(now: u32, deadline: u32) -> bool { now.wrapping_sub(deadline) < u32::MAX / 2 }

fn log_line(line: impl Into<String>) -> Fx<AppHost> { Fx::Log(LogLine(line.into())) }

impl DeepLink {
    /// One frame of the link. Pure: no IO, no globals; every line and request is returned.
    pub(crate) fn step(&mut self, event: LinkEvent, env: &LinkEnv) -> LinkOut {
        let mut out = LinkOut::default();
        if let LinkEvent::Arrived(parsed) = event {
            match parsed {
                // A launch without a link (the launcher's own icon) leaves one still waiting alone.
                Parsed::None => {}
                Parsed::Bad => out.fx.push(log_line("deeplink: not opened (bad ratingKey)")),
                Parsed::Link(link) => {
                    out.fx.push(log_line(format!(
                        "deeplink: {:?} rk={} server={}", link.action, link.rk,
                        if link.server.is_some() { "named" } else { "current" })));
                    self.0 = LinkState::Waiting { link, server_deadline: None, crossed_sign_in: false };
                }
            }
        }
        let LinkState::Waiting { link, server_deadline, crossed_sign_in } = &mut self.0 else { return out };
        match env.root {
            Root::Login => {
                *crossed_sign_in = true;
                *server_deadline = None;
                return out;
            }
            Root::Other => {
                *server_deadline = None;
                return out;
            }
            Root::Home => {}
        }
        if link.server.is_none() && *crossed_sign_in {
            self.0 = LinkState::Idle;
            out.fx.push(log_line("deeplink: dropped (signed in after the link arrived; it names no server)"));
            return out;
        }
        let deadline = *server_deadline.get_or_insert(env.now.wrapping_add(SERVER_WAIT_MS));
        let sid = match link.server.as_deref() {
            Some(machine) => env.roster.by_machine(machine),
            None => env.roster.current(),
        };
        let Some(sid) = sid else {
            if expired(env.now, deadline) {
                self.0 = LinkState::Idle;
                out.fx.push(log_line("deeplink: not opened (server not available)"));
            }
            return out;
        };
        if !env.foreground || env.surface_up {
            return out;
        }
        // Home alone underneath, so BACK off the page lands there: unwind whatever stands above it
        // and wait for that to settle before asking.
        if !env.home_settled {
            if !env.nav_pending {
                out.fx.push(Fx::Nav(NavOp::SelectTab(AppArg::Home)));
            }
            return out;
        }
        let LinkState::Waiting { link, .. } = std::mem::take(&mut self.0) else { return out };
        out.fx.push(Fx::App(AppFx::Store(
            plx_data::stores::StoreId::Metadata,
            plx_data::stores::StoreCmd::Metadata(plx_data::stores::metadata::MetadataCmd::RequestDetail {
                sid, rk: link.rk.clone() }),
        )));
        out.arm = Some((sid, link.rk, link.action == Action::Open));
        out
    }

    #[cfg(test)]
    pub(crate) fn waiting_for_test(link: Link, server_deadline: Option<u32>, crossed_sign_in: bool) -> Self {
        DeepLink(LinkState::Waiting { link, server_deadline, crossed_sign_in })
    }

    /// The route rig's "settled" check.
    #[cfg(test)]
    pub(crate) fn is_idle(&self) -> bool { matches!(self.0, LinkState::Idle) }

    #[cfg(test)]
    fn is_crossed(&self) -> bool {
        matches!(self.0, LinkState::Waiting { crossed_sign_in: true, .. })
    }

    #[cfg(test)]
    fn server_deadline(&self) -> Option<u32> {
        match self.0 { LinkState::Waiting { server_deadline, .. } => server_deadline, _ => None }
    }
}

/// Production's roster: the one place the link reads the servers registry.
pub(crate) struct GlobalRoster;

impl LinkRoster for GlobalRoster {
    fn by_machine(&self, machine: &str) -> Option<ServerId> { plx_plex::plex::id_of_machine(machine) }
    fn current(&self) -> Option<ServerId> {
        plx_plex::plex::client_opt().map(|_| plx_plex::plex::current_server())
    }
}

/// **The one function that reads the outside world for the link:** the launch slot (a payload that
/// arrived, read by pure [`parse`]), the window, the nav stack and the roster port.
pub(crate) fn link_inputs(
    pages: &plx_ui::dispatch::Dispatcher<AppHost>,
    foreground: bool,
    now: u32,
) -> (LinkEvent, LinkEnv<'static>) {
    let event = match plx_platform::tv::launch::take() {
        Some(raw) => LinkEvent::Arrived(parse(&raw)),
        None => LinkEvent::Frame,
    };
    let root = match pages.nav.tabs.stack.entries.first().map(|e| &e.arg) {
        Some(AppArg::Login) => Root::Login,
        Some(AppArg::Home) => Root::Home,
        _ => Root::Other,
    };
    (event, LinkEnv {
        now,
        foreground,
        root,
        surface_up: pages.surface_up(),
        nav_pending: pages.has_pending_navigation(),
        home_settled: super::bridge::top_settled_on(pages, &AppArg::Home),
        roster: &GlobalRoster,
    })
}

/// Hand a step's output to the executors that already exist. It makes no decisions: a structural
/// op goes through `Dispatcher::request`, any other effect through `Dispatcher::emit`, and the
/// wait is the one app-loop write.
pub(crate) fn perform(
    out: LinkOut,
    pages: &mut plx_ui::dispatch::Dispatcher<AppHost>,
    menu_play_await: &mut Option<super::input::MenuPlayAwait>,
    now: u32,
) {
    for fx in out.fx {
        match fx {
            Fx::Nav(op) => pages.request(MachineId::Nav, op),
            other => pages.emit(MachineId::Nav, other),
        }
    }
    if let Some((sid, rk, open)) = out.arm {
        *menu_play_await = Some(super::input::MenuPlayAwait::for_link(sid, rk, open, now));
    }
}

/// One frame of the link, run just before `menu_play_tick`: read, step, perform.
pub(crate) fn frame(
    pages: &mut plx_ui::dispatch::Dispatcher<AppHost>,
    link: &mut DeepLink,
    menu_play_await: &mut Option<super::input::MenuPlayAwait>,
    foreground: bool,
    now: u32,
) {
    let (event, env) = link_inputs(pages, foreground, now);
    let out = link.step(event, &env);
    perform(out, pages, menu_play_await, now);
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

    // The link's pure step: a fixture roster and env, one event in, the exact state and effects out.

    struct FixedRoster {
        named: Option<(&'static str, ServerId)>,
        current: Option<ServerId>,
    }

    impl LinkRoster for FixedRoster {
        fn by_machine(&self, machine: &str) -> Option<ServerId> {
            self.named.filter(|(m, _)| *m == machine).map(|(_, sid)| sid)
        }
        fn current(&self) -> Option<ServerId> { self.current }
    }

    const KNOWN: FixedRoster = FixedRoster { named: Some(("abc", ServerId::UNSET)), current: Some(ServerId::UNSET) };
    const EMPTY: FixedRoster = FixedRoster { named: None, current: None };

    fn env(roster: &FixedRoster) -> LinkEnv<'_> {
        LinkEnv {
            now: 1_000,
            foreground: true,
            root: Root::Home,
            surface_up: false,
            nav_pending: false,
            home_settled: true,
            roster,
        }
    }

    fn fx_names(fx: &[Fx<AppHost>]) -> Vec<String> {
        fx.iter().map(|f| match f {
            Fx::Log(LogLine(line)) => format!("log:{line}"),
            Fx::Nav(NavOp::SelectTab(AppArg::Home)) => "select-home".to_owned(),
            Fx::App(AppFx::Store(
                plx_data::stores::StoreId::Metadata,
                plx_data::stores::StoreCmd::Metadata(plx_data::stores::metadata::MetadataCmd::RequestDetail { rk, .. }),
            )) => format!("request-detail:{rk}"),
            _ => panic!("the link emitted an effect it must not"),
        }).collect()
    }

    fn waiting(server: Option<&str>, crossed: bool) -> DeepLink {
        DeepLink::waiting_for_test(
            Link { server: server.map(str::to_owned), rk: "7".into(), action: Action::Open }, None, crossed)
    }

    #[test]
    fn link_step_login_waits_and_marks_the_sign_in() {
        let mut link = waiting(None, false);
        let mut e = env(&EMPTY);
        e.root = Root::Login;
        let out = link.step(LinkEvent::Frame, &e);
        assert!(fx_names(&out.fx).is_empty() && out.arm.is_none());
        assert!(link.is_crossed() && !link.is_idle());
    }

    #[test]
    fn link_step_the_picker_and_onboarding_wait_without_marking_a_sign_in() {
        let mut link = waiting(None, false);
        let mut e = env(&KNOWN);
        e.root = Root::Other;
        let out = link.step(LinkEvent::Frame, &e);
        assert!(fx_names(&out.fx).is_empty() && out.arm.is_none());
        assert!(!link.is_crossed() && !link.is_idle() && link.server_deadline().is_none());
    }

    #[test]
    fn link_step_a_serverless_link_that_crossed_a_sign_in_is_dropped() {
        let mut link = waiting(None, true);
        let out = link.step(LinkEvent::Frame, &env(&KNOWN));
        assert_eq!(fx_names(&out.fx),
            ["log:deeplink: dropped (signed in after the link arrived; it names no server)"]);
        assert!(out.arm.is_none() && link.is_idle());
    }

    #[test]
    fn link_step_a_named_link_that_crossed_a_sign_in_lands() {
        let mut link = waiting(Some("abc"), true);
        let out = link.step(LinkEvent::Frame, &env(&KNOWN));
        assert_eq!(fx_names(&out.fx), ["request-detail:7"]);
        assert!(matches!(out.arm, Some((_, ref rk, true)) if rk == "7") && link.is_idle());
    }

    #[test]
    fn link_step_the_server_wait_starts_at_home_and_expires() {
        let mut link = waiting(Some("zzz"), false);
        let mut e = env(&KNOWN);
        let out = link.step(LinkEvent::Frame, &e);
        assert!(fx_names(&out.fx).is_empty() && !link.is_idle());
        assert_eq!(link.server_deadline(), Some(1_000 + SERVER_WAIT_MS));
        e.now = 1_000 + SERVER_WAIT_MS;
        let out = link.step(LinkEvent::Frame, &e);
        assert_eq!(fx_names(&out.fx), ["log:deeplink: not opened (server not available)"]);
        assert!(link.is_idle());
    }

    #[test]
    fn link_step_the_server_wait_runs_under_a_surface() {
        let mut link = waiting(Some("zzz"), false);
        let mut e = env(&KNOWN);
        e.surface_up = true;
        link.step(LinkEvent::Frame, &e);
        assert_eq!(link.server_deadline(), Some(1_000 + SERVER_WAIT_MS));
    }

    #[test]
    fn link_step_waits_under_a_surface_and_while_backgrounded() {
        for (surface_up, foreground) in [(true, true), (false, false), (true, false)] {
            let mut link = waiting(None, false);
            let mut e = env(&KNOWN);
            e.surface_up = surface_up;
            e.foreground = foreground;
            e.home_settled = false;
            let out = link.step(LinkEvent::Frame, &e);
            assert!(fx_names(&out.fx).is_empty() && out.arm.is_none() && !link.is_idle(),
                "surface_up={surface_up} foreground={foreground}");
        }
    }

    #[test]
    fn link_step_unwinds_to_home_once_and_waits_for_the_commit() {
        let mut link = waiting(None, false);
        let mut e = env(&KNOWN);
        e.home_settled = false;
        assert_eq!(fx_names(&link.step(LinkEvent::Frame, &e).fx), ["select-home"]);
        e.nav_pending = true;
        assert!(fx_names(&link.step(LinkEvent::Frame, &e).fx).is_empty());
        assert!(!link.is_idle());
    }

    #[test]
    fn link_step_the_landing_requests_the_item_and_arms_the_wait() {
        let mut link = waiting(None, false);
        let out = link.step(LinkEvent::Frame, &env(&KNOWN));
        assert_eq!(fx_names(&out.fx), ["request-detail:7"]);
        assert!(matches!(out.arm, Some((_, ref rk, true)) if rk == "7") && link.is_idle());
    }

    #[test]
    fn link_step_an_arrival_logs_and_a_bad_key_changes_nothing() {
        let mut link = DeepLink::default();
        let mut e = env(&KNOWN);
        e.root = Root::Other;
        let arrived = LinkEvent::Arrived(Parsed::Link(Link { server: None, rk: "9".into(), action: Action::Play }));
        let out = link.step(arrived, &e);
        assert_eq!(fx_names(&out.fx), ["log:deeplink: Play rk=9 server=current"]);
        assert!(!link.is_idle());
        let out = link.step(LinkEvent::Arrived(Parsed::Bad), &e);
        assert_eq!(fx_names(&out.fx), ["log:deeplink: not opened (bad ratingKey)"]);
        let out = link.step(LinkEvent::Arrived(Parsed::None), &e);
        assert!(fx_names(&out.fx).is_empty() && !link.is_idle());
    }

    #[test]
    fn deadlines_wrap() {
        assert!(!expired(100, 200));
        assert!(expired(200, 200));
        assert!(expired(5, u32::MAX - 5));
    }
}
