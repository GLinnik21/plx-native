//! `/tmp/plxnative-tls-selftest` — watch the wrong-clock TLS fallback (`net::keypin`, issue #378)
//! work, or refuse, on the real television with no account behind it.
//!
//! The developer boot (`--guest`, a token from a trigger file) talks plain HTTP to a compiled-in
//! host, never runs the `/identity` discovery probe and holds no session, so none of the
//! fallback's inputs exist there. This trigger supplies them from one JSON object and then
//! exercises BOTH planes the fallback lives in, round after round, so a clock that is changed (or
//! a certificate that is swapped) mid-run shows up as a line that flips from `mode=strict` to
//! `mode=key`.
//!
//! ```jsonc
//! {"origin":"https://<dashed-ip>.<hash>.plex.direct:32400",
//!  "ip":"<lan-ip>", "pin":"sha256//<pin>", "rounds":60, "interval_s":5}
//! ```
//!
//! * `origin` — required, `https` only.
//! * `ip` — optional. Builds the [`ResolvePin`] the app builds for a plex.direct name (it must be
//!   the address the dashed host label encodes, exactly as in a stored session); without it the
//!   name resolves through DNS and the identity probe is not a LEARNING one.
//! * `pin` — optional. Put in the key table before round 1: how a deliberately wrong pin, or a
//!   wrong-name origin, is tested. Without it the pin is learned from the first strictly verified
//!   `/identity` answer that carries one.
//! * `rounds` / `interval_s` — optional, clamped to [`MAX_ROUNDS`] / [`MAX_INTERVAL_S`].
//!
//! **What it never does:** touch the session, the account or any authenticated endpoint. It sends
//! no token, and its table entry is made with `keypin::note_machine_pin` + `keypin::bind` only —
//! never `learn_server_key` or `grade_learning`, which write the stored session.
//!
//! **What it never logs:** the origin, the address or a pin value. Each round writes one line per
//! plane (`tls-selftest r=N control: …` / `media: …`) and the transport's own `net:` / `curlio:`
//! lines, which carry the X509 verify result in words, sit next to them in the same log. A
//! refusal here names only the libcurl code, because the control plane's failure value carries
//! nothing more.

use crate::net::{keypin, resolve};
use crate::plex::{Origin, ResolvePin};
use std::time::{Duration, Instant};

/// Upper bounds, so a typo cannot park a thread (and a TV's network) for a day.
pub(crate) const MAX_ROUNDS: u32 = 720;
pub(crate) const MAX_INTERVAL_S: u32 = 300;
const DEFAULT_ROUNDS: u32 = 12;
const DEFAULT_INTERVAL_S: u32 = 5;
/// How long one plane may take before its round gives up.
const PLANE_BUDGET: Duration = Duration::from_secs(15);
/// The media plane reads this much of `/identity`: a few hundred bytes is the whole body.
const READ_BYTES: usize = 512;

/// The parsed trigger. Deliberately not `Debug`: `origin`, `ip` and `pin` are exactly what must
/// never reach a log.
pub(crate) struct Config {
    origin: Origin,
    ip: Option<String>,
    pin: Option<String>,
    rounds: u32,
    interval_s: u32,
}

#[derive(serde::Deserialize)]
struct Raw {
    #[serde(default)]
    origin: String,
    #[serde(default)]
    ip: Option<String>,
    #[serde(default)]
    pin: Option<String>,
    #[serde(default)]
    rounds: Option<u32>,
    #[serde(default)]
    interval_s: Option<u32>,
}

/// Parse the trigger's value. The `Err` is a fixed phrase, never an echo of the input: serde's own
/// messages quote the offending value, and the value here is a lab address and a key pin.
pub(crate) fn parse(s: &str) -> Result<Config, &'static str> {
    let raw: Raw = serde_json::from_str(s).map_err(|_| "not a JSON object of the expected shape")?;
    let origin = Origin::parse(raw.origin.trim()).ok_or("origin is not an http(s) origin")?;
    if !origin.is_tls() {
        return Err("origin is not https");
    }
    let ip = match raw.ip.map(|v| v.trim().to_owned()).filter(|v| !v.is_empty()) {
        Some(v) if v.trim_matches(|c| c == '[' || c == ']').parse::<std::net::IpAddr>().is_ok() => Some(v),
        Some(_) => return Err("ip is not an address"),
        None => None,
    };
    let pin = match raw.pin.map(|v| v.trim().to_owned()).filter(|v| !v.is_empty()) {
        Some(v) if v.starts_with("sha256//") && v.len() > "sha256//".len() => Some(v),
        Some(_) => return Err("pin is not sha256//..."),
        None => None,
    };
    Ok(Config {
        origin,
        ip,
        pin,
        rounds: raw.rounds.unwrap_or(DEFAULT_ROUNDS).clamp(1, MAX_ROUNDS),
        interval_s: raw.interval_s.unwrap_or(DEFAULT_INTERVAL_S).clamp(1, MAX_INTERVAL_S),
    })
}

/// What one plane's round came to: the log line and the two facts the end line counts.
pub(crate) struct Plane {
    pub(crate) line: String,
    pub(crate) ok: bool,
    pub(crate) key: bool,
}

/// One self-test run's state.
pub(crate) struct Selftest {
    cfg: Config,
    resolve_pin: Option<ResolvePin>,
    /// `host:port` in [`keypin`]'s tables, and the synthetic machine this run's pin is filed under —
    /// one per `host:port`, so a second run in one process (the host tests) cannot re-arm another's.
    key: String,
    machine: String,
    /// The pin this run holds in the table: given, or learned.
    held: Option<String>,
}

impl Selftest {
    pub(crate) fn new(cfg: Config) -> Selftest {
        let key = keypin::key_of(cfg.origin.host(), cfg.origin.port());
        let resolve_pin = cfg.ip.as_deref().and_then(|ip| ResolvePin::for_origin(&cfg.origin, ip));
        if let Some(pin) = &resolve_pin {
            // The media plane cannot be handed a pin per request: it asks this table by host:port.
            resolve::add(pin);
        }
        let me = Selftest { machine: format!("tls-selftest-{key}"), key, resolve_pin, held: cfg.pin.clone(), cfg };
        me.assert_table();
        me
    }

    pub(crate) fn rounds(&self) -> u32 {
        self.cfg.rounds
    }

    pub(crate) fn interval(&self) -> Duration {
        Duration::from_secs(u64::from(self.cfg.interval_s))
    }

    pub(crate) fn start_line(&self) -> String {
        format!(
            "tls-selftest armed rounds={} interval_s={} pin_given={} resolve_pin={}",
            self.cfg.rounds,
            self.cfg.interval_s,
            yes_no(self.cfg.pin.is_some()),
            yes_no(self.resolve_pin.is_some()),
        )
    }

    /// (Re)state the held pin in the key table. Done before every plane because
    /// `keypin::project` replaces the session's remembered keys wholesale and would take this one
    /// with it (the pin is filed under a synthetic machine no session holds), so a session write
    /// that lands mid-round costs at most the plane it overlaps; stating an unchanged pin again
    /// does not disturb the latch.
    fn assert_table(&self) {
        if let Some(pin) = &self.held {
            keypin::bind(&self.machine, self.cfg.origin.host(), self.cfg.origin.port());
            keypin::note_machine_pin(&self.machine, pin);
        }
    }

    /// What the log lines report as `pin_held`: whether the key table holds a pin for this origin
    /// at the moment the line is written (not whether this run was given or learned one).
    fn pin_held(&self) -> &'static str {
        yes_no(keypin::holds(&self.key))
    }

    /// One round: the control plane, then the media plane, the table re-stated before each.
    pub(crate) fn round(&mut self, r: u32) -> [Plane; 2] {
        self.round_with(r, || ())
    }

    /// [`round`](Self::round) with `between` run after the control plane and before the table is
    /// re-stated for the media plane: where a session write landing mid-round would fall.
    fn round_with(&mut self, r: u32, between: impl FnOnce()) -> [Plane; 2] {
        self.assert_table();
        let control = self.control(r);
        between();
        self.assert_table();
        let media = self.media(r);
        [control, media]
    }

    /// The mode the request that just ended was answered in. A strict success clears the host's
    /// latch and a key-mode success sets it, so after an answer the latch IS the mode.
    fn mode(&self) -> &'static str {
        if keypin::is_latched(&self.key) { "key" } else { "strict" }
    }

    fn control(&mut self, r: u32) -> Plane {
        let reply = crate::auth::get_identity(&self.cfg.origin, self.resolve_pin.as_ref(), PLANE_BUDGET);
        match reply {
            crate::auth::ProbeReply::Answered { status, peer_pin, .. } => {
                let mode = self.mode();
                let ok = (200..300).contains(&status);
                let mut line = if ok {
                    format!("tls-selftest r={r} control: ok mode={mode}")
                } else {
                    format!("tls-selftest r={r} control: answered status={status} mode={mode}")
                };
                if mode == "strict" {
                    line.push_str(&format!(" learned={}", yes_no(peer_pin.is_some())));
                    if let (Some(learned), Some(given)) = (&peer_pin, &self.cfg.pin) {
                        line.push_str(&format!(" matches_given={}", yes_no(learned == given)));
                    }
                    if let (Some(learned), None) = (peer_pin, &self.held) {
                        self.held = Some(learned);
                        self.assert_table();
                    }
                }
                line.push_str(&format!(" pin_held={}", self.pin_held()));
                Plane { line, ok, key: mode == "key" }
            }
            crate::auth::ProbeReply::Failed(failure) => {
                let rc = failure.and_then(|f| f.curl_rc).map_or("none".to_owned(), |rc| rc.to_string());
                Plane { line: format!("tls-selftest r={r} control: refused rc={rc} pin_held={}", self.pin_held()), ok: false, key: false }
            }
        }
    }

    fn media(&self, r: u32) -> Plane {
        use crate::curlio::{CurlSource, OpenErr};
        let refused = |what: String| Plane { line: format!("tls-selftest r={r} media: refused {what} pin_held={}", self.pin_held()), ok: false, key: false };
        let reservation = match CurlSource::reserve_open() {
            Ok(res) => res,
            Err(e) => return refused(format!("open={e:?}")),
        };
        let url = format!("{}/identity", self.cfg.origin.base());
        let until = Instant::now() + PLANE_BUDGET;
        let mut src = match CurlSource::open_reserved_checked(&url, 0, reservation, Some(until), &mut crate::checkpoint::NoCheckpoint) {
            Ok(src) => src,
            Err(OpenErr::Transport(rc)) => return refused(format!("rc={rc}")),
            Err(e) => return refused(format!("open={e:?}")),
        };
        let mut buf = [0u8; READ_BYTES];
        let n = src.read_until(&mut buf, Some(until), &mut crate::checkpoint::NoCheckpoint);
        if n <= 0 {
            return refused(format!("read={n}"));
        }
        let mode = self.mode();
        Plane { line: format!("tls-selftest r={r} media: ok mode={mode} bytes={n} pin_held={}", self.pin_held()), ok: true, key: mode == "key" }
    }
}

fn yes_no(b: bool) -> &'static str {
    if b { "yes" } else { "no" }
}

/// The trigger's one entry point, called once from `app::boot` after libcurl is bound AND after the
/// boot session projection (`session::project_server_keys`): that projection replaces the key table
/// wholesale, and this run's pin is filed under a synthetic machine no session holds, so arming
/// earlier would let the projection wipe it under round 1's first handshake. A no-op without the
/// trigger. The worker is detached and ends with its rounds.
pub(crate) fn arm_at_boot() {
    let Some(value) = crate::devtrig::read("tls-selftest") else { return };
    let cfg = match parse(&value) {
        Ok(cfg) => cfg,
        Err(why) => {
            crate::eventlog::log(&format!("tls-selftest IGNORED — {why}"));
            return;
        }
    };
    let mut run = Selftest::new(cfg);
    crate::eventlog::log(&run.start_line());
    if crate::task::spawn("tls-selftest", move || drive(&mut run)).is_none() {
        crate::eventlog::log("tls-selftest IGNORED — the worker thread could not start");
    }
}

fn drive(run: &mut Selftest) {
    let (mut ok, mut key) = ([0u32; 2], [0u32; 2]);
    for r in 1..=run.rounds() {
        for (i, plane) in run.round(r).into_iter().enumerate() {
            crate::eventlog::log(&plane.line);
            ok[i] += u32::from(plane.ok);
            key[i] += u32::from(plane.key);
        }
        if r < run.rounds() {
            std::thread::sleep(run.interval());
        }
    }
    crate::eventlog::log(&format!(
        "tls-selftest done rounds={} control_ok={} control_key={} media_ok={} media_key={}",
        run.rounds(), ok[0], key[0], ok[1], key[1]
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    const HASH: &str = "0123456789abcdef0123456789abcdef";

    fn host() -> String {
        format!("127-0-0-1.{HASH}.plex.direct")
    }

    fn value(extra: &str) -> String {
        format!(r#"{{"origin":"https://{}:32400"{extra}}}"#, host())
    }

    #[test]
    fn a_minimal_value_arms_with_the_defaults() {
        let cfg = parse(&value("")).ok().expect("origin alone arms");
        assert_eq!((cfg.rounds, cfg.interval_s), (DEFAULT_ROUNDS, DEFAULT_INTERVAL_S));
        assert!(cfg.ip.is_none() && cfg.pin.is_none());
    }

    #[test]
    fn rounds_and_interval_are_clamped_not_trusted() {
        let cfg = parse(&value(r#","rounds":100000,"interval_s":86400"#)).ok().unwrap();
        assert_eq!((cfg.rounds, cfg.interval_s), (MAX_ROUNDS, MAX_INTERVAL_S));
        let cfg = parse(&value(r#","rounds":0,"interval_s":0"#)).ok().unwrap();
        assert_eq!((cfg.rounds, cfg.interval_s), (1, 1));
        let cfg = parse(&value(r#","rounds":60,"interval_s":5,"ip":"127.0.0.1","pin":"sha256//abc=""#)).ok().unwrap();
        assert_eq!((cfg.rounds, cfg.interval_s), (60, 5));
        assert!(cfg.ip.is_some() && cfg.pin.is_some());
    }

    #[test]
    fn garbage_is_not_armed_and_the_reason_never_echoes_the_input() {
        for bad in [
            "",
            "not json",
            "[]",
            "{}",
            r#"{"origin":"http://127.0.0.1:32400"}"#,
            r#"{"origin":"ftp://x"}"#,
            r#"{"rounds":"SECRETVALUE"}"#,
            r#"{"origin":"https://127-0-0-1.h.plex.direct:32400","rounds":"SECRETVALUE"}"#,
            r#"{"origin":"https://127-0-0-1.h.plex.direct:32400","ip":"SECRETVALUE"}"#,
            r#"{"origin":"https://127-0-0-1.h.plex.direct:32400","pin":"SECRETVALUE"}"#,
        ] {
            match parse(bad) {
                Ok(_) => panic!("armed on {bad:?}"),
                Err(why) => assert!(!why.contains("SECRETVALUE"), "{why}"),
            }
        }
    }

    // ---- one loopback round through the real transports ----------------------------------------

    fn serve(not_before: i64, not_after: i64, tag: &str) -> (Arc<crate::net::TestCert>, crate::net::TestCaGuard, u16) {
        let cert = Arc::new(crate::net::mint_ca_issued_cert(&[&host()], crate::net::ymd_from_now(not_before), crate::net::ymd_from_now(not_after)));
        let ca = crate::net::TestCaGuard::install(&cert.pem, tag);
        let body = br#"{"MediaContainer":{"machineIdentifier":"selftest"}}"#.to_vec();
        let port = crate::net::spawn_dual_protocol(Arc::clone(&cert), body);
        (cert, ca, port)
    }

    fn run_for(port: u16, extra: &str) -> Selftest {
        let value = format!(r#"{{"origin":"https://{}:{port}","ip":"127.0.0.1"{extra}}}"#, host());
        Selftest::new(parse(&value).ok().expect("a loopback value parses"))
    }

    fn forget(port: u16) {
        keypin::forget_for_test(&keypin::key_of(&host(), i32::from(port)));
    }

    /// A strictly verified round learns the server's key into the table (and nowhere else), and
    /// both planes say `strict`.
    #[test]
    fn a_strict_round_learns_the_pin_and_both_planes_say_strict() {
        let _serial = crate::testlock::serial();
        if !(crate::net::global_init() && crate::net::available()) { return; }
        resolve::clear();
        let (cert, _ca, port) = serve(-30, 30, "selftest-strict");
        let given = crate::spki::pin_from_spki_der(&cert.spki_der);
        // `pin` absent: the pin is learned. A second run with it given reports `matches_given`.
        let mut run = run_for(port, "");
        let [control, media] = run.round(1);
        assert_eq!(control.line, "tls-selftest r=1 control: ok mode=strict learned=yes pin_held=yes", "{}", control.line);
        assert!(media.line.starts_with("tls-selftest r=1 media: ok mode=strict bytes="), "{}", media.line);
        assert!(control.ok && media.ok && !control.key && !media.key);
        assert_eq!(run.held.as_deref(), Some(given.as_str()), "the learned pin is the served leaf's");
        assert_eq!(keypin::pin_for_test(&run.key).as_deref(), Some(given.as_str()), "and it is in the table");
        let [again, _] = run.round(2);
        assert!(again.line.ends_with("pin_held=yes"), "{}", again.line);
        forget(port);

        let mut run = run_for(port, &format!(r#","pin":"{given}""#));
        let [control, _] = run.round(1);
        assert!(control.line.contains("learned=yes matches_given=yes"), "{}", control.line);
        forget(port);
        resolve::clear();
    }

    /// The device clock is wrong (an expired leaf), the pin is the one a strict round learned from
    /// the same kind of server: both planes answer in key mode.
    #[test]
    fn an_expired_leaf_with_a_held_pin_is_answered_in_key_mode_on_both_planes() {
        let _serial = crate::testlock::serial();
        if !(crate::net::global_init() && crate::net::available()) { return; }
        resolve::clear();
        let (cert, _ca, port) = serve(-90, -30, "selftest-expired");
        let pin = crate::spki::pin_from_spki_der(&cert.spki_der);
        let mut run = run_for(port, &format!(r#","pin":"{pin}""#));
        let [control, media] = run.round(7);
        assert_eq!(control.line, "tls-selftest r=7 control: ok mode=key pin_held=yes", "{}", control.line);
        assert!(media.line.starts_with("tls-selftest r=7 media: ok mode=key bytes="), "{}", media.line);
        assert!(control.key && media.key);
        forget(port);
        resolve::clear();
    }

    /// The round-1 refusal sequence on a television whose clock is already wrong at launch: the
    /// table is stated, a session projection replaces it wholesale (this run's pin
    /// is filed under a machine no session holds), and the handshake that follows finds no pin.
    /// Arming after the boot projection (`app::boot`) is what keeps this off the real boot; what
    /// the run itself guarantees is that a wipe costs one plane at most and is reported truthfully
    /// (`pin_held` is the table, not the run's own "given or learned" field).
    #[test]
    fn a_projection_that_wipes_the_pin_costs_one_plane_and_the_line_says_so() {
        let _serial = crate::testlock::serial();
        if !(crate::net::global_init() && crate::net::available()) { return; }
        resolve::clear();
        let (cert, _ca, port) = serve(-90, -30, "selftest-wiped");
        let pin = crate::spki::pin_from_spki_der(&cert.spki_der);
        let mut run = run_for(port, &format!(r#","pin":"{pin}""#));
        assert!(keypin::holds(&run.key), "stated at construction");
        let wipe = || crate::plex::session::project_server_keys(&crate::plex::session::Session::default(), false);

        // The projection lands before the first handshake: the control plane is refused, and the
        // line does not claim a pin the table no longer holds.
        wipe();
        assert!(!keypin::holds(&run.key), "the projection took the pin with it");
        let control = run.control(1);
        assert_eq!(control.line, "tls-selftest r=1 control: refused rc=60 pin_held=no", "{}", control.line);

        // A projection landing between the planes: the control plane (pin stated at round start)
        // answers in key mode, and the media plane, re-stated after the wipe, does too.
        let [control, media] = run.round_with(2, wipe);
        assert_eq!(control.line, "tls-selftest r=2 control: ok mode=key pin_held=yes", "{}", control.line);
        assert!(media.line.starts_with("tls-selftest r=2 media: ok mode=key bytes="), "{}", media.line);
        assert!(media.line.ends_with("pin_held=yes"), "{}", media.line);
        forget(port);
        resolve::clear();
    }

    /// Without a pin there is nothing to recognise the server by: the date refuses it and the
    /// line says so, and a WRONG pin is a pin mismatch (rc 90), never an answer.
    #[test]
    fn an_expired_leaf_is_refused_without_a_pin_and_with_a_wrong_one() {
        let _serial = crate::testlock::serial();
        if !(crate::net::global_init() && crate::net::available()) { return; }
        resolve::clear();
        let (_cert, _ca, port) = serve(-90, -30, "selftest-refused");
        let mut run = run_for(port, "");
        let [control, media] = run.round(1);
        assert_eq!(control.line, "tls-selftest r=1 control: refused rc=60 pin_held=no", "{}", control.line);
        assert!(media.line.starts_with("tls-selftest r=1 media: refused rc=60"), "{}", media.line);
        assert!(!control.ok && !media.ok);
        forget(port);

        let other = crate::net::mint_cert(&["127.0.0.1"]);
        let wrong = crate::spki::pin_from_spki_der(&other.spki_der);
        let mut run = run_for(port, &format!(r#","pin":"{wrong}""#));
        let [control, media] = run.round(2);
        assert_eq!(control.line, "tls-selftest r=2 control: refused rc=90 pin_held=yes", "{}", control.line);
        assert!(media.line.starts_with("tls-selftest r=2 media: refused rc=90"), "{}", media.line);
        forget(port);
        resolve::clear();
    }
}
