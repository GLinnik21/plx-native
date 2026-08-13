//! The `/tmp` developer-trigger surface, behind one door.
//!
//! This app is driven headlessly by ~44 files under `/tmp/plxnative-*`: which screen to boot to,
//! which item to play, which URL to stream, whether to auto-press OK, which PMS token to use.
//! That is how `tests/run.py` and every capture scene work, and it is not going away.
//!
//! It must not exist in a public build. `/tmp` is the SHARED system `/tmp` in the production jail
//! too (mode 1777, both jail profiles), so on an ordinary user's TV every one of those files is a
//! behaviour switch any co-resident process can throw. Three are outright takeovers:
//! `plxnative-token` beats the signed-in session (`app.rs`'s boot gate), `plxnative-servers` hands
//! the app a whole additional server — an address AND the token to trust it with (see [`servers`])
//! — and `plxnative-url` replaces the stream the player feeds.
//!
//! So every read goes through here, and here is `#[cfg]`-gated on the `devtriggers` feature. In a
//! `--no-default-features` build [`flag`] is `false` and [`read`] is `None` at COMPILE time, the
//! branches behind them fold away, and the binary opens nothing under `/tmp` but its own logs.
//!
//! Two rules for anything added later:
//!
//! 1. **Never open a `/tmp` path directly.** The grep that audits this (`/tmp/plxnative-` outside
//!    this module and the four log sinks) is the only thing keeping the property true.
//! 2. **A gate is not always a path.** `any_trigger_present` scans the whole directory and names
//!    no file at all — it was the one surface a literal-replacement sweep would have missed, and
//!    it silently changes which screen the app boots to. Structural surfaces that take no name
//!    (the capture listener's `INADDR_ANY` socket, the remote FIFO's `mkfifo`) are gated at their
//!    call sites in `app.rs` for the same reason.
//!
//! The four LOG sinks are deliberately NOT here and stay in every build: they are creates, not
//! reads, they are how on-device crash triage works at all, and writing them is not a way for
//! another process to steer this one.

/// Files that are pure diagnostics rather than automation — see [`any_trigger_present`].
///
/// Every log this app writes belongs here, not just its trigger. `plxnative-anim` was listed and
/// `plxnative-anim.log` was not, while arming the overlay creates exactly that file and nothing
/// ever removes it (`make run` clears only the event log; `tests/run.py` spares every `*.log` by
/// design) — so a single historical anim session skipped the who's-watching picker on every later
/// boot, interactive ones included.
// `test` as well as the feature: `any_trigger_present` is the only caller and it is cfg'd out of a
// release build, but the test below asserts this list's contents and runs with default features.
#[cfg(any(feature = "devtriggers", test))]
const DIAG: [&str; 9] = [
    "plxnative-events.log",
    "plxnative-stderr.log",
    "plxnative-crash.log",
    "plxnative-anim.log",
    "plxnative-profile",
    "plxnative-anim",
    "plxnative-remote",
    "plxnative-capture",
    "plxnative-noidle",
];

/// Is the trigger `name` (bare, without the `plxnative-` prefix) present?
#[cfg(feature = "devtriggers")]
pub(crate) fn flag(name: &str) -> bool {
    std::path::Path::new(&path(name)).exists()
}
#[cfg(not(feature = "devtriggers"))]
pub(crate) fn flag(_name: &str) -> bool {
    false
}

/// The trigger's CONTENT, trimmed. `Some("")` for a trigger armed as an empty file — several
/// distinguish empty (take the default) from a value (`autoseek`, `library`, `marker`), so an
/// empty file must not read the same as an absent one.
#[cfg(feature = "devtriggers")]
pub(crate) fn read(name: &str) -> Option<String> {
    std::fs::read_to_string(path(name)).ok().map(|s| s.trim().to_string())
}
#[cfg(not(feature = "devtriggers"))]
pub(crate) fn read(_name: &str) -> Option<String> {
    None
}

/// A raw dev payload by ABSOLUTE path — only `/tmp/sample.h264` and `/tmp/sample.h265`, which
/// predate the `plxnative-` prefix and feed the player a local Annex-B sample instead of a stream.
#[cfg(feature = "devtriggers")]
pub(crate) fn read_bytes_at(abs: &str) -> Option<Vec<u8>> {
    std::fs::read(abs).ok()
}
#[cfg(not(feature = "devtriggers"))]
pub(crate) fn read_bytes_at(_abs: &str) -> Option<Vec<u8>> {
    None
}

/// One ADDITIONAL server's credentials, injected for an automated run — see [`servers`].
///
/// Deliberately **not** `Debug`: `token` is a live per-(user,server) PMS access token, and a
/// derived `Debug` is exactly how a secret reaches a log by accident. [`DevServer::describe`] is
/// the only formatter this type has, and it prints everything *but* the token.
#[derive(serde::Deserialize, Clone)]
pub(crate) struct DevServer {
    /// Display name. Cosmetic — what a server picker would label it.
    #[serde(default)]
    pub(crate) name: String,
    /// The server's `machineIdentifier`: its IDENTITY, and the only thing that distinguishes it
    /// from the primary once both are installed. Public, not a secret.
    #[serde(default)]
    pub(crate) machine_id: String,
    /// Address reachable **from the TV**. `address` is accepted as an alias because that is what
    /// `plex::session::ServerRef` calls the same field, and copying one into the other by hand is
    /// the obvious way to write this file.
    #[serde(default, alias = "address")]
    pub(crate) host: String,
    #[serde(default = "default_port")]
    pub(crate) port: i64,
    /// This identity's per-(user,server) access token **for this server**. A shared server is a
    /// separate authority: the account token gets a 401 from it, which is the whole reason one
    /// `plxnative-token` cannot express two servers. A SECRET — never logged.
    #[serde(default)]
    pub(crate) token: String,
    /// The owner's plex.tv handle (`sourceTitle` on the wire) — "bamx23". EMPTY means **your own
    /// server**, which is what the Sources list draws as the absence of an owner rather than as an
    /// anonymous one, so a harness overlay that omits it injects an owned server by definition.
    /// Public, like the machine name: it is the string every browsing surface says out loud.
    #[serde(default, alias = "sourceTitle", alias = "source_title")]
    pub(crate) handle: String,
}

fn default_port() -> i64 {
    32400
}

impl DevServer {
    /// Everything about this server except the token, for the event log.
    pub(crate) fn describe(&self) -> String {
        // by CHARS, not bytes: a machineIdentifier is hex in practice, but a hand-written file is
        // whatever someone typed, and slicing a byte range mid-codepoint panics.
        let mut mid: String = self.machine_id.chars().take(8).collect();
        if self.machine_id.chars().nth(8).is_some() {
            mid.push_str("..");
        }
        format!("name={:?} handle={:?} {}:{} mid={}", self.name, self.handle, self.host, self.port, mid)
    }
    /// Are these credentials complete enough to reach the server at all?
    pub(crate) fn usable(&self) -> bool {
        !self.host.is_empty() && !self.token.is_empty() && self.port > 0
    }
}

/// Parse the `servers` trigger's content: a JSON array of [`DevServer`], or a single bare object.
///
/// An `Err` rather than an empty list on malformed JSON, deliberately — a run that injected
/// credentials and got them silently dropped would grade as "the feature is broken", when the real
/// fault is a typo in the harness overlay. The caller logs the parse error.
#[cfg(any(feature = "devtriggers", test))]
fn parse_servers(s: &str) -> Result<Vec<DevServer>, String> {
    // Many FIRST: `untagged` tries the variants in order, and a derived struct deserializer also
    // accepts a SEQUENCE (positional fields), so with `One` first an empty `[]` came back as one
    // all-defaults server instead of no servers at all.
    #[derive(serde::Deserialize)]
    #[serde(untagged)]
    enum ManyOrOne {
        Many(Vec<DevServer>),
        One(DevServer),
    }
    if s.trim().is_empty() {
        return Ok(Vec::new()); // an empty file means "no extra servers", not a syntax error
    }
    match serde_json::from_str::<ManyOrOne>(s) {
        Ok(ManyOrOne::Many(v)) => Ok(v),
        Ok(ManyOrOne::One(d)) => Ok(vec![d]),
        Err(e) => Err(e.to_string()),
    }
}

/// The EXTRA servers this boot was given credentials for — `/tmp/plxnative-servers`.
///
/// Purely ADDITIVE: the primary server is still the compiled-in host plus `plxnative-token` (or the
/// signed-in session), untouched, so a run that names one server behaves exactly as it always has.
/// This is the channel for the *second* authority — a friend's shared server, which has its own
/// `machineIdentifier` and its own access token and answers 401 to anybody else's.
///
/// Read and parsed **once**. `tests/run.py` wipes `/tmp/plxnative-*` between cases and again on
/// exit (pass, fail or Ctrl-C — that is how a live token stops surviving in world-readable `/tmp`),
/// so a second read could legitimately see the file gone. The credentials a boot was handed are a
/// property of that boot, not of what `/tmp` happens to hold when someone asks.
#[cfg(feature = "devtriggers")]
pub(crate) fn servers() -> Result<Vec<DevServer>, String> {
    static ONCE: std::sync::OnceLock<Result<Vec<DevServer>, String>> = std::sync::OnceLock::new();
    ONCE.get_or_init(|| match read("servers") {
        Some(s) => parse_servers(&s),
        None => Ok(Vec::new()),
    })
    .clone()
}
#[cfg(not(feature = "devtriggers"))]
pub(crate) fn servers() -> Result<Vec<DevServer>, String> {
    Ok(Vec::new())
}

/// Is ANY non-diagnostic trigger armed? Used to skip the boot who's-watching picker, so that a
/// headless run lands on a deterministic Home.
///
/// This is the surface with no path literal: it `read_dir`s the shared `/tmp` and matches by
/// prefix, so in a release build it would still have run — and still have changed the boot
/// screen from a squatted file — after every named read had been compiled out.
#[cfg(feature = "devtriggers")]
pub(crate) fn any_trigger_present() -> bool {
    std::fs::read_dir("/tmp")
        .ok()
        .map(|rd| {
            rd.filter_map(|e| e.ok()).any(|e| {
                let n = e.file_name().to_string_lossy().into_owned();
                n.starts_with("plxnative-") && !DIAG.contains(&n.as_str())
            })
        })
        .unwrap_or(false)
}
#[cfg(not(feature = "devtriggers"))]
pub(crate) fn any_trigger_present() -> bool {
    false
}

/// `true` when this build reads `/tmp` at all — for the one boot log line that says so, and for
/// call sites gating a whole subsystem (the capture listener, the remote FIFO) rather than a read.
pub(crate) const ENABLED: bool = cfg!(feature = "devtriggers");

#[cfg(feature = "devtriggers")]
fn path(name: &str) -> String {
    format!("/tmp/plxnative-{name}")
}

#[cfg(test)]
mod tests {
    /// The DIAG list must name every log the app writes, or that log permanently suppresses the
    /// boot picker. This asserts the property against the paths the code actually opens rather
    /// than against a copy of the list, so adding a fifth log sink without listing it fails here.
    #[test]
    fn diag_names_every_log_this_app_writes() {
        for log in ["plxnative-events.log", "plxnative-stderr.log", "plxnative-crash.log", "plxnative-anim.log"] {
            assert!(super::DIAG.contains(&log), "{log} is written by this app but absent from DIAG — it would suppress the boot picker forever");
        }
    }

    /// An empty trigger file and an absent one mean different things to several call sites
    /// (`autoseek` empty = one seek to 140s; `navosc` empty = Home <-> the first library section).
    #[test]
    fn empty_trigger_is_some_not_none() {
        if !super::ENABLED {
            return; // a release build reads nothing; nothing to distinguish
        }
        // `/tmp` by literal, NOT `env::temp_dir()`: on the dev Mac that is `$TMPDIR`, a per-user
        // `/var/folders/…/T/` path, while `path()` always builds `/tmp/plxnative-…` — so the write
        // and the read never met and this failed on the host for a reason having nothing to do
        // with the property under test.
        let p = std::path::Path::new("/tmp/plxnative-devtest-empty");
        std::fs::write(p, "").unwrap();
        let got = super::read("devtest-empty");
        let _ = std::fs::remove_file(p);
        assert_eq!(got.as_deref(), Some(""), "an empty trigger must not read as absent");
    }

    /// The wire format the harness writes: a JSON ARRAY of servers, and — because a human arming
    /// this by hand will write one server — a bare OBJECT too.
    #[test]
    fn servers_parse_array_and_single_object() {
        let arr = r#"[{"name":"Mine","machine_id":"aaaa1111bbbb","host":"10.0.0.2","port":32400,
                       "token":"t1"},
                      {"name":"Friend","machine_id":"cccc2222dddd","host":"10.0.0.9","port":32401,
                       "token":"t2"}]"#;
        let v = super::parse_servers(arr).expect("array must parse");
        assert_eq!(v.len(), 2);
        assert_eq!(v[1].host, "10.0.0.9");
        assert_eq!(v[1].port, 32401);
        assert!(v.iter().all(|s| s.usable()));

        let one = r#"{"name":"Friend","host":"10.0.0.9","token":"t2"}"#;
        let v = super::parse_servers(one).expect("a bare object must parse too");
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].port, 32400, "port must default to the PMS port, not 0");
    }

    /// The harness's payload, verbatim — `tests/run.py::shared_servers_json` emits exactly this
    /// (compact, an array of one, these SIX keys). It is the only automated link between the two
    /// halves of the mechanism: rename a field on either side and this fails instead of a device
    /// run quietly booting with one server.
    ///
    /// **`handle` is the load-bearing one**, and it is the reason this assertion is worth more than
    /// it looks. `app.rs` derives `owned = handle.is_empty()` from it, which decides whether the
    /// injected server's libraries are pinned to Home and whether they get tab pills of their own
    /// (`browse::tabs`). Drop or rename it on the Python side and every injected server silently
    /// becomes one of YOURS — a friend's libraries pinned and pilled — with nothing else failing.
    #[test]
    fn servers_parse_the_harness_payload_verbatim() {
        let payload = concat!(
            r#"[{"name":"Bob's Plex","machine_id":"friend222","handle":"bob","host":"10.0.0.9","#,
            r#""port":32400,"token":"FRIENDTOK"}]"#
        );
        let v = super::parse_servers(payload).expect("the harness payload must parse");
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].name, "Bob's Plex");
        assert_eq!(v[0].machine_id, "friend222");
        assert_eq!(v[0].handle, "bob", "the owner's handle — an empty one means YOUR OWN server");
        assert_eq!(v[0].host, "10.0.0.9");
        assert_eq!(v[0].port, 32400);
        assert!(v[0].usable());

        // …and the wire spelling plex.tv itself uses, so a hand-written file copied straight off a
        // /api/v2/resources row works too
        let wire = r#"[{"sourceTitle":"bob","host":"10.0.0.9","token":"t"}]"#;
        assert_eq!(super::parse_servers(wire).unwrap()[0].handle, "bob");
    }

    /// `address` is what `plex::session::ServerRef` calls the host field, so a file written by
    /// copying one is the likeliest hand-authored shape there is.
    #[test]
    fn servers_accept_address_as_host_alias() {
        let v = super::parse_servers(r#"[{"address":"10.0.0.9","token":"t"}]"#).unwrap();
        assert_eq!(v[0].host, "10.0.0.9");
        assert!(v[0].usable());
    }

    /// Malformed JSON must be an ERROR, never an empty list: a run that injected credentials and
    /// had them silently dropped looks like a broken feature instead of a typo'd overlay.
    #[test]
    fn servers_malformed_is_an_error_not_an_empty_list() {
        assert!(super::parse_servers("{not json").is_err());
        assert!(super::parse_servers("").unwrap().is_empty(), "an empty file = no extra servers");
        assert!(super::parse_servers("[]").unwrap().is_empty());
        // …and the error text goes to the EVENT LOG, so it must not quote the input back. It is a
        // half-written credentials file: the byte after the truncation could be the token.
        // (matched rather than `unwrap_err`, which wants `T: Debug` — the absent derive this type
        // relies on for its no-secret-in-a-log property is load-bearing right here.)
        let e = match super::parse_servers(r#"[{"token":"SECRETTOKENVALUE""#) {
            Err(e) => e,
            Ok(_) => panic!("truncated JSON must not parse"),
        };
        assert!(!e.contains("SECRETTOKENVALUE"), "the parse error echoed the input: {e}");
    }

    /// Half a credential is worse than none — it reaches the server and 401s. The boot log says so
    /// per entry rather than installing it.
    #[test]
    fn servers_incomplete_credentials_are_not_usable() {
        let v = super::parse_servers(
            r#"[{"host":"10.0.0.9"},{"token":"t"},{"host":"10.0.0.9","port":0,"token":"t"}]"#,
        )
        .unwrap();
        assert!(v.iter().all(|s| !s.usable()), "no-token / no-host / no-port must all be rejected");
    }

    /// The one formatter this type has must not be a way for a token to reach the event log.
    #[test]
    fn describe_never_carries_the_token() {
        let v = super::parse_servers(
            r#"[{"name":"Friend","machine_id":"0123456789abcdef","host":"10.0.0.9",
                 "token":"SECRETTOKENVALUE"}]"#,
        )
        .unwrap();
        let d = v[0].describe();
        assert!(!d.contains("SECRETTOKENVALUE"), "describe() leaked the token: {d}");
        assert!(d.contains("10.0.0.9:32400"), "{d}");
        assert!(d.contains("mid=01234567.."), "the machine id is truncated, not dropped: {d}");
    }

    /// `plxnative-servers` must NOT be exempt from the picker-suppression scan: it names a host and
    /// carries the token to trust it with, which is automation of the strongest kind. Listing it in
    /// DIAG would let a headless run boot to the who's-watching picker instead of Home.
    #[test]
    fn servers_trigger_is_not_diagnostic() {
        assert!(!super::DIAG.contains(&"plxnative-servers"));
    }
}
