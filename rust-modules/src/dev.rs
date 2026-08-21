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
//!    this module and the four unconditional log sinks) is the only thing keeping the property
//!    true. The two profiler logs are dev-only and listed in [`DIAG`] below.
//! 2. **A gate is not always a path.** `any_trigger_present` scans the whole directory and names
//!    no file at all — it was the one surface a literal-replacement sweep would have missed, and
//!    it silently changes which screen the app boots to. Structural surfaces that take no name
//!    (the capture listener's `INADDR_ANY` socket, the remote FIFO's `mkfifo`) are gated at their
//!    call sites in `app.rs` for the same reason.
//!
//! The four unconditional LOG sinks are deliberately NOT here and stay in every build: they are creates, not
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
const DIAG: [&str; 13] = [
    "plxnative-events.log",
    "plxnative-stderr.log",
    "plxnative-crash.log",
    "plxnative-anim.log",
    "plxnative-profile",
    "plxnative-gputime.jsonl",
    "plxnative-hwcnt",
    "plxnative-hwcnt.jsonl",
    "plxnative-anim",
    "plxnative-remote",
    "plxnative-capture",
    "plxnative-noidle",
    // The focus fingerprint ([`crate::focusprobe`]). Diagnostic for `noidle`'s reason and one of
    // its own: it only READS focus and writes a log line, and a (route × key) characterization
    // harness has to be able to observe the who's-watching picker, which a non-DIAG trigger would
    // suppress — the observer would remove the screen it was armed to watch.
    "plxnative-focus",
];

/// Is the trigger `name` (bare, without the `plxnative-` prefix) present?
#[cfg(feature = "devtriggers")]
pub(crate) fn flag(name: &str) -> bool {
    path(name).exists()
}
#[cfg(not(feature = "devtriggers"))]
pub(crate) fn flag(_name: &str) -> bool {
    false
}

/// [`flag`], answered ONCE for the whole process.
///
/// **A `dev::flag` is a `stat`, so a trigger read every frame is a syscall on the 60 fps path.**
/// Latching also fixes a correctness wrinkle that has nothing to do with cost: `tests/run.py`
/// clears `/tmp/plxnative-*` between cases, so a later read can legitimately find the file gone
/// mid-run and a per-frame probe would change its answer half way through a case.
///
/// A macro rather than a function because the latch has to be a `static` per trigger, and a
/// function would need a map behind a lock — which is the thing being avoided. It lives HERE
/// because this module is the one door onto the `/tmp` surface; it was briefly a file-local macro
/// in `ui/widgets.rs`, which walled it off from the other per-frame `flag` callers
/// ([`crate::focusprobe::armed`] had already hand-rolled exactly this body, doc comment and all).
///
/// No `#[cfg]` arms, deliberately: [`flag`] is already `false` at COMPILE time without the
/// `devtriggers` feature, so a second gate here would only re-derive what the door behind it
/// guarantees.
///
/// ```ignore
/// crate::dev::latched_flag!(
///     /// `/tmp/plxnative-flattabs` — the material off, for an A/B against the flat capsule.
///     fn flat_tabs_armed = "flattabs";
/// );
/// ```
macro_rules! latched_flag {
    ($(#[$m:meta])* $vis:vis fn $name:ident = $trigger:literal;) => {
        $(#[$m])*
        $vis fn $name() -> bool {
            static SEEN: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
            *SEEN.get_or_init(|| $crate::dev::flag($trigger))
        }
    };
}
pub(crate) use latched_flag;

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
    /// The owner's plex.tv handle (`sourceTitle` on the wire) — "friend". EMPTY means **your own
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
        // A SHARED server is someone else's machine, and this line goes to
        // `/tmp/plxnative-events.log` — the file that gets pasted into issues and PR bodies. Four
        // PR bodies leaked exactly these fields on 2026-08-14 and had to be redacted after the
        // fact, which a public repository does not really allow. So a share names NOTHING that
        // identifies it or its owner: not the server's name, not the plex.tv handle, not the
        // address, not the machineIdentifier.
        //
        // The token was already excluded (`describe_never_carries_the_token`), but a token is not
        // the only thing here worth protecting — an address plus a handle is a person's home.
        //
        // What survives is what DEBUGGING actually needs: that a share is present at all, whether
        // its credentials are complete, and a stable `ref` so two lines about the same server can
        // be correlated within one log without identifying it outside one. An OWNED server is the
        // user's own machine, already in the boot line and in `config.local.h`, so it is unchanged.
        if !self.handle.is_empty() {
            return format!("SHARED ref={} port_set={}", self.reference(), self.port > 0);
        }
        // by CHARS, not bytes: a machineIdentifier is hex in practice, but a hand-written file is
        // whatever someone typed, and slicing a byte range mid-codepoint panics.
        let mut mid: String = self.machine_id.chars().take(8).collect();
        if self.machine_id.chars().nth(8).is_some() {
            mid.push_str("..");
        }
        format!("name={:?} handle={:?} {}:{} mid={}", self.name, self.handle, self.host, self.port, mid)
    }

    /// A short, stable, NON-reversible tag for a shared server — enough to tell two shares apart in
    /// one log and to follow one across a boot, and useless to anyone reading that log elsewhere.
    ///
    /// FNV-1a over the machineIdentifier: not a cryptographic choice, a legibility one. The id is
    /// the right input because it is the only field that survives the server changing address.
    fn reference(&self) -> String {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in self.machine_id.as_bytes() {
            h ^= *b as u64;
            h = h.wrapping_mul(0x1000_0000_01b3);
        }
        format!("{:06x}", h & 0xff_ffff)
    }
    /// Are these credentials complete enough to reach the server at all?
    ///
    /// The port is judged by [`crate::plex::probe::dial_port`], not by `> 0`: `app.rs` registers
    /// every server that passes this with `s.port as c_int`, and this file is a hand-written JSON
    /// blob under `/tmp` — an out-of-range `i64` wraps in that cast into a port nobody wrote down.
    pub(crate) fn usable(&self) -> bool {
        !self.host.is_empty() && !self.token.is_empty() && crate::plex::probe::dial_port(self.port).is_some()
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
    std::fs::read_dir(crate::paths::runtime_dir())
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

/// The trigger's absolute path. `/tmp/plxnative-<name>` on the television; see
/// [`crate::paths::runtime_dir`] for why a host build may put the whole namespace elsewhere.
///
/// `test` is in the cfg beside the feature, and only for a compile reason: the test below writes
/// through this door rather than through a literal, and it guards itself at RUNTIME on
/// [`ENABLED`] — but a runtime guard cannot stop a call from being compiled, so without this the
/// whole crate failed to build under `--no-default-features --test` (E0425, "cannot find function
/// `path` in module `super`"). A shipping release build is unchanged: `cfg(test)` is false there,
/// and the fn is gone exactly as before.
#[cfg(any(feature = "devtriggers", test))]
fn path(name: &str) -> std::path::PathBuf {
    crate::paths::in_runtime_dir(&format!("plxnative-{name}"))
}

#[cfg(test)]
mod tests {
    /// The DIAG list must name every log the app writes, or that log permanently suppresses the
    /// boot picker. This asserts the property against the paths the code actually opens rather
    /// than against a copy of the list, so adding another log sink without listing it fails here.
    #[test]
    fn diag_names_every_log_this_app_writes() {
        for log in [
            "plxnative-events.log",
            "plxnative-stderr.log",
            "plxnative-crash.log",
            "plxnative-anim.log",
            "plxnative-gputime.jsonl",
            "plxnative-hwcnt.jsonl",
        ] {
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
        // Write through `path()` itself, NOT a literal and NOT `env::temp_dir()`. The literal was
        // right when the namespace was always `/tmp/plxnative-…`, but it stops meeting the read as
        // soon as an instance root is in effect; `env::temp_dir()` never met it at all, since on
        // the dev Mac that is a per-user `/var/folders/…/T/` path. Going through the same door the
        // code under test uses keeps the write and the read together wherever the root points.
        let p = super::path("devtest-empty");
        std::fs::write(&p, "").unwrap();
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
    ///
    /// The last entry is the one that is not obviously broken: `app.rs` registers whatever passes
    /// this with `s.port as c_int`, and `4_294_999_696 as i32` is **32400**, so a hand-written
    /// number no port can be would have installed a server pointing at the most ordinary port
    /// there is. Judged by `plex::probe::dial_port`, it is refused like the rest.
    #[test]
    fn servers_incomplete_credentials_are_not_usable() {
        let v = super::parse_servers(
            r#"[{"host":"10.0.0.9"},{"token":"t"},{"host":"10.0.0.9","port":0,"token":"t"},
                {"host":"10.0.0.9","port":4294999696,"token":"t"}]"#,
        )
        .unwrap();
        assert!(v.iter().all(|s| !s.usable()), "no-token / no-host / no-port / a port that wraps");
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

    /// …and a SHARED server names nothing at all. The event log is pasted into public issues and PR
    /// bodies — it has already happened, to these exact fields — so a share's owner, machine and
    /// address must not be in it. The token was never the only thing worth protecting here.
    #[test]
    fn describe_redacts_everything_identifying_about_someone_elses_server() {
        let v = super::parse_servers(
            r#"[{"name":"Film Club","machine_id":"0123456789abcdef","host":"10.9.9.7",
                 "port":31234,"token":"SECRETTOKENVALUE","handle":"friend"}]"#,
        )
        .unwrap();
        let d = v[0].describe();
        for leak in ["SECRETTOKENVALUE", "Film Club", "friend", "10.9.9.7", "31234", "0123456789", "01234567"] {
            assert!(!d.contains(leak), "describe() leaked {leak:?}: {d}");
        }
        assert!(d.contains("SHARED"), "a share still says it is one: {d}");
        assert!(d.contains("port_set=true"), "…and that its credentials look complete: {d}");
    }

    /// The correlation tag is stable for one server and different for another — the whole point of
    /// keeping a tag rather than dropping the field. A reader can follow one share across a boot
    /// and tell two shares apart, and learn nothing about either.
    #[test]
    fn the_shared_reference_is_stable_per_machine_and_distinct_across_them() {
        let mk = |mid: &str| {
            super::parse_servers(&format!(
                r#"[{{"machine_id":"{mid}","host":"10.9.9.7","token":"t","handle":"friend"}}]"#
            ))
            .unwrap()[0]
                .describe()
        };
        assert_eq!(mk("aaaaaaaaaaaa"), mk("aaaaaaaaaaaa"), "same machine, same tag");
        assert_ne!(mk("aaaaaaaaaaaa"), mk("bbbbbbbbbbbb"), "two shares must be tellable apart");
    }

    /// The tag is computed in TWO languages: `tests/run.py`'s `server_ref`/`describe_server` print
    /// the same line for the same server, so a harness transcript and an event log can be read as
    /// one story — and so that the harness, which also prints to a pasteable stream, is held to
    /// this module's redaction contract rather than to its own.
    ///
    /// Nothing links the two implementations at build time, so the whole line is pinned to a
    /// literal here. **If this assertion has to change, `run.py`'s copy changes with it.**
    #[test]
    fn the_shared_reference_is_the_same_tag_the_harness_prints() {
        let v = super::parse_servers(
            r#"[{"name":"Film Club","machine_id":"abcd1234efgh","host":"10.9.9.7",
                 "port":31234,"token":"t","handle":"friend"}]"#,
        )
        .unwrap();
        assert_eq!(v[0].describe(), "SHARED ref=71c955 port_set=true");
    }

    /// `plxnative-servers` must NOT be exempt from the picker-suppression scan: it names a host and
    /// carries the token to trust it with, which is automation of the strongest kind. Listing it in
    /// DIAG would let a headless run boot to the who's-watching picker instead of Home.
    #[test]
    fn servers_trigger_is_not_diagnostic() {
        assert!(!super::DIAG.contains(&"plxnative-servers"));
    }
}
