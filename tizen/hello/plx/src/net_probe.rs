//! Probe H8 (step 9 "net"): does the application's libcurl binding load on this set, and can it reach a
//! server on the tester's PC?
//!
//! What it calls, and why those:
//!
//! - **`plx_net::net::curl::load(None)`**, the app's own `dynlib!` table over the app's own SONAME list
//!   (`libcurl.so.4`, `libcurl.so.5`, `libcurl.4.dylib`; `rust-modules/net/src/net.rs`, on top of
//!   `rust-modules/base/src/dynlib.rs`). It says which name opened, or that none did, or that one opened
//!   with symbols missing. Nothing is linked: `libplx.so` has no DT_NEEDED for curl.
//! - **`plx_net::net::global_init()`**, which the app calls once on its main thread at boot. It loads the
//!   table (again, harmlessly), calls `curl_global_init`, sets up the legacy OpenSSL locks where the TLS
//!   backend needs them and logs the version. Here it is called at the end of `plx_tizen_create`, on the
//!   host's main thread, before any worker exists.
//! - **`plx_net::net::curl_version()`** for the version string (it names the TLS backend, which is the
//!   fact worth having from a set nobody here owns).
//! - **`plx_net::net::request_result_evidence`**, the one function every libcurl easy request in the app
//!   passes through, for `GET http://<mock-host>/identity` on a worker thread (the app's rule: a request
//!   never runs on the thread that draws). It gives the HTTP status and body, or on a failure the
//!   `CURLcode` the transfer returned; [`curl_error_text`] turns that into words.
//!
//! The mock server is `tests/mock_pms.py` (its `/identity` is tokenless JSON with `machineIdentifier`):
//! see the README, "H8". The address comes from `res/mock-host.txt`, which `build.sh --mock-host` writes
//! exactly as `--log-host` writes `res/log-host.txt`; this library reads it from the package's `res/`
//! directory itself, so the C# host and the native ABI are unchanged.
//!
//! Nothing here can crash the app or skip the panel: the whole of [`start`] runs under `catch_unwind`, the
//! request is on its own thread under `catch_unwind`, and every outcome, a panic included, ends as a panel
//! line and a log line.

use crate::log::plog;
use crate::panel::{self, Step};
use plx_base::dynlib::Loaded;
use std::panic::{catch_unwind, AssertUnwindSafe};

/// The file in `res/` that carries the mock server's `HOST:PORT` (`build.sh --mock-host`).
pub const MOCK_FILE: &str = "mock-host.txt";

/// A copy of the SONAME list `plx_net` binds (`rust-modules/net/src/net.rs`), for the panel's "tried ..."
/// text only: the list itself is private to that crate. A test reads `net.rs` and fails if the two differ.
pub const CANDIDATES: [&str; 3] = ["libcurl.so.4", "libcurl.so.5", "libcurl.4.dylib"];

/// Longest panel line this probe writes: its own bound, well under the panel's runaway guard
/// (`panel::STEP_LINE_MAX`, 400). A `9 net` line is the verdict and a short reason; the URL, the load
/// result and the version are also logged on their own `H8 net:` lines.
const LINE_MAX: usize = 108;
/// The identity body is a few hundred bytes; anything past this is not it.
const BODY_MAX: usize = 64 * 1024;

fn cap(s: &str, max: usize) -> String {
    if s.chars().count() > max {
        let mut cut: String = s.chars().take(max.saturating_sub(3)).collect();
        cut.push_str("...");
        cut
    } else {
        s.to_string()
    }
}

// --- the mock host ---------------------------------------------------------------------------------

/// `res/mock-host.txt`, as read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MockHost {
    /// No file: the package was built without `--mock-host`.
    Absent,
    /// A file that is not `HOST:PORT` (what it held, flattened and cut).
    Bad(String),
    /// A well-formed `HOST:PORT`.
    Set(String),
}

/// `HOST:PORT`: an IPv4 literal (each octet 0-255) or a DNS name (letters, digits, `-` and `.`, not starting
/// or ending with either, no empty label), then `:` and a port 1-65535. Trailing whitespace and a final
/// newline are ignored (the file is written with one). No IPv6 and no `http://` prefix: the probe builds the
/// URL itself, and a scheme or a path here would send the request somewhere the tester did not mean.
pub fn parse_mock_host(raw: &str) -> Result<String, String> {
    let s = raw.trim();
    let (host, port) = s.rsplit_once(':').ok_or_else(|| "expected HOST:PORT, e.g. 10.0.2.2:32499".to_string())?;
    let port: u16 = port.parse().ok().filter(|p| *p != 0).ok_or_else(|| format!("port {port:?} is not 1-65535"))?;
    if host.is_empty() {
        return Err("empty host".to_string());
    }
    let numeric = host.chars().all(|c| c.is_ascii_digit() || c == '.');
    if numeric {
        let octets: Vec<&str> = host.split('.').collect();
        let ok = octets.len() == 4 && octets.iter().all(|o| !o.is_empty() && o.len() <= 3 && o.parse::<u8>().is_ok());
        if !ok {
            return Err(format!("{host:?} is not an IPv4 address"));
        }
    } else {
        let label_ok = |l: &str| !l.is_empty() && !l.starts_with('-') && !l.ends_with('-') && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
        if !host.split('.').all(label_ok) {
            return Err(format!("{host:?} is not a host name"));
        }
    }
    Ok(format!("{host}:{port}"))
}

/// Read `<res_dir>/mock-host.txt`.
pub fn read_mock_host(res_dir: Option<&str>) -> MockHost {
    let Some(dir) = res_dir.filter(|d| !d.is_empty()) else { return MockHost::Absent };
    let path = std::path::Path::new(dir).join(MOCK_FILE);
    match std::fs::read_to_string(&path) {
        Ok(raw) => match parse_mock_host(&raw) {
            Ok(h) => MockHost::Set(h),
            Err(why) => {
                plog!("H8 net: {} is not HOST:PORT ({why}): {:?}", path.display(), cap(raw.trim(), 60));
                MockHost::Bad(panel::clean(raw.trim()))
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => MockHost::Absent,
        Err(e) => {
            plog!("H8 net: cannot read {}: {e}", path.display());
            MockHost::Bad(format!("unreadable: {e}"))
        }
    }
}

// --- reading the answer ----------------------------------------------------------------------------

/// The `machineIdentifier` of an `/identity` answer: the mock's JSON (`"machineIdentifier": "<id>"`) or a
/// real server's XML (`machineIdentifier="<id>"`). A plain scan, so no JSON parser is needed; the value
/// must be a non-empty string of at most 64 printable characters, else `None`.
pub fn machine_identifier(body: &str) -> Option<String> {
    let at = body.find("machineIdentifier")?;
    let rest = body[at + "machineIdentifier".len()..].trim_start_matches(['"', '\'']);
    let rest = rest.trim_start();
    let rest = rest.strip_prefix([':', '=']).unwrap_or(rest).trim_start();
    let rest = rest.strip_prefix(['"', '\''])?;
    let end = rest.find(['"', '\''])?;
    let id = &rest[..end];
    (!id.is_empty() && id.chars().count() <= 64 && id.chars().all(|c| !c.is_control())).then(|| id.to_string())
}

/// Words for the `CURLcode`s a LAN GET can end in (`libcurl-errors`); any other number is named as a number,
/// not guessed at.
pub fn curl_error_text(rc: i32) -> String {
    let text = match rc {
        1 => "unsupported protocol",
        3 => "malformed URL",
        6 => "could not resolve host",
        7 => "failed to connect to host",
        18 => "partial transfer",
        23 => "write error",
        28 => "operation timed out",
        35 => "SSL connect error",
        52 => "empty reply from server",
        55 => "send failure",
        56 => "receive failure (connection reset)",
        60 => "peer certificate cannot be verified",
        _ => return format!("curl error {rc}"),
    };
    format!("curl error {rc}: {text}")
}

// --- the report -------------------------------------------------------------------------------------

/// What libcurl's load did, read once on the main thread.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CurlFacts {
    /// The SONAME that opened with every symbol resolved, or why none did.
    pub loaded: Result<&'static str, String>,
    /// `curl_global_init` and the TLS setup ran (`net::global_init`).
    pub init_ok: bool,
    /// `curl_version()`.
    pub version: Option<String>,
}

/// How the GET ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GetResult {
    Reply { status: u16, body: Vec<u8> },
    /// `rc` is the `CURLcode` (None: refused before libcurl ran).
    Failed { rc: Option<i32>, timed_out: bool },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetReport {
    pub curl: CurlFacts,
    pub mock: MockHost,
    /// `None` while the request is still running, or when none was made.
    pub get: Option<GetResult>,
    /// The request was started and has not finished.
    pub pending: bool,
}

impl NetReport {
    /// The URL the probe asks for, when there is a well-formed mock host.
    pub fn url(&self) -> Option<String> {
        match &self.mock {
            MockHost::Set(h) => Some(format!("http://{h}/identity")),
            _ => None,
        }
    }

    /// The panel's two lines and whether the step is green. Line 1 is the verdict, line 2 which libcurl and
    /// its version. Each is at most [`LINE_MAX`] characters; the verdict is first so a cut loses detail, not
    /// the answer.
    pub fn lines(&self) -> (bool, [String; 2]) {
        let l2 = match &self.curl.loaded {
            Ok(so) => format!("libcurl {so}: {}", self.curl.version.as_deref().unwrap_or("version unreadable")),
            Err(why) => format!("libcurl NOT loaded: {why}"),
        };
        let target = self.url().map(|u| u.trim_start_matches("http://").to_string());
        let (ok, l1) = match (&self.curl.loaded, self.curl.init_ok) {
            (Err(_), _) => (false, "FAILED: libcurl did not load, so no request was made".to_string()),
            (Ok(_), false) => (false, "FAILED: libcurl loaded but curl_global_init / the TLS setup failed".to_string()),
            (Ok(_), true) => match (&self.mock, &self.get, self.pending) {
                (MockHost::Absent, ..) => (true, "libcurl LOADED; no GET made (this package has no mock host: build with --mock-host)".to_string()),
                (MockHost::Bad(v), ..) => (false, format!("FAILED: res/mock-host.txt is not HOST:PORT: {}", cap(v, 40))),
                (MockHost::Set(_), None, _) => {
                    (true, format!("libcurl LOADED; GET http://{}{}", target.unwrap_or_default(), if self.pending { " in progress" } else { " not started" }))
                }
                (MockHost::Set(_), Some(GetResult::Reply { status, body }), _) => {
                    let t = target.unwrap_or_default();
                    let text = String::from_utf8_lossy(body);
                    match (*status, machine_identifier(&text)) {
                        (200, Some(id)) => (true, format!("OK: GET http://{t} -> 200, machineIdentifier {id}")),
                        (200, None) => (false, format!("NOT OK: GET http://{t} -> 200 but no machineIdentifier in {} bytes", body.len())),
                        (s, _) => (false, format!("NOT OK: GET http://{t} -> HTTP {s}, not the mock's 200")),
                    }
                }
                (MockHost::Set(_), Some(GetResult::Failed { rc, timed_out }), _) => {
                    let why = match rc {
                        Some(rc) => curl_error_text(*rc),
                        None if *timed_out => "timed out".to_string(),
                        None => "refused before libcurl ran".to_string(),
                    };
                    (false, format!("FAILED: GET http://{}: {why}", target.unwrap_or_default()))
                }
            },
        };
        (ok, [cap(&l1, LINE_MAX), cap(&l2, LINE_MAX)])
    }
}

// --- running it -------------------------------------------------------------------------------------

/// Load libcurl and run the global init. MAIN thread, once, before any worker uses curl.
pub fn load_curl() -> CurlFacts {
    let loaded = match plx_net::net::curl::load(None) {
        Loaded::Ok(so) => Ok(so),
        Loaded::NoLibrary => Err(format!("none of {} opened", CANDIDATES.join(" "))),
        Loaded::Incomplete(so, n) => Err(format!("{so} opened but lacks {n} required symbol(s)")),
    };
    plog!("H8 net: curl load -> {loaded:?}");
    let init_ok = loaded.is_ok() && plx_net::net::global_init();
    let version = init_ok
        .then(|| {
            // SAFETY: libcurl is bound (`init_ok`); `curl_version` returns a static string, read once here
            // on the main thread before any request thread exists (libcurl rewrites its buffer per call).
            let p = unsafe { plx_net::net::curl_version() };
            (!p.is_null()).then(|| unsafe { std::ffi::CStr::from_ptr(p) }.to_string_lossy().into_owned())
        })
        .flatten();
    plog!("H8 net: global_init -> {init_ok}; curl_version {version:?}; curl_version_num 0x{:06x}", plx_net::net::curl_version_num());
    CurlFacts { loaded, init_ok, version }
}

/// One `GET url` through the app's own request function.
pub fn real_get(url: &str) -> GetResult {
    match plx_net::net::request_result_evidence(url, &[], "GET", None, plx_net::net::API, false, Some(BODY_MAX), None, false) {
        Ok(r) => GetResult::Reply { status: r.status, body: r.body },
        Err(f) => GetResult::Failed { rc: f.curl_rc, timed_out: f.cause == plx_net::net::RequestError::TimedOut },
    }
}

fn publish(report: &NetReport, provisional: bool) {
    let (ok, lines) = report.lines();
    plog!("{} {} | {}", if provisional { "H8 net:" } else { "H8 RESULT:" }, lines[0], lines[1]);
    panel::step_lines(Step::Net, ok, &lines);
}

/// The whole step: report the load at once, and when there is a mock host make the GET on a worker thread
/// and report that when it ends. `get` is the request (the real one, or a test's). Returns the worker's
/// handle when one was started, for a test to join.
pub fn run(curl: CurlFacts, mock: MockHost, get: fn(&str) -> GetResult) -> Option<std::thread::JoinHandle<NetReport>> {
    let mut report = NetReport { curl, mock, get: None, pending: false };
    let startable = report.curl.loaded.is_ok() && report.curl.init_ok && report.url().is_some();
    report.pending = startable;
    publish(&report, startable);
    let url = report.url().filter(|_| startable)?;
    let spawned = std::thread::Builder::new().name("plx-net-probe".into()).spawn(move || {
        plog!("H8 net: GET {url}");
        let result = catch_unwind(AssertUnwindSafe(|| get(&url))).unwrap_or_else(|_| {
            plog!("H8 net: the request PANICKED");
            GetResult::Failed { rc: None, timed_out: false }
        });
        report.get = Some(result);
        report.pending = false;
        publish(&report, false);
        // libcurl's own log lines (the app's event log, kept in memory) now reach the UDP log too.
        crate::gfx_probe::forward_events();
        report
    });
    match spawned {
        Ok(h) => Some(h),
        Err(e) => {
            plog!("H8 net: cannot spawn the request thread: {e}");
            panel::step_lines(Step::Net, false, &[format!("FAILED: cannot start the request thread: {}", cap(&e.to_string(), 50)), String::new()]);
            None
        }
    }
}

/// Called at the end of `plx_tizen_create` (main thread). Never fails the create and never skips the panel.
pub fn start(res_dir: Option<&str>) {
    if catch_unwind(AssertUnwindSafe(|| {
        let mock = read_mock_host(res_dir);
        plog!("H8 net: mock host {mock:?}");
        let curl = load_curl();
        // The app's own log lines from the load (which SONAME bound, the TLS backend) reach the UDP log.
        crate::gfx_probe::forward_events();
        run(curl, mock, real_get);
    }))
    .is_err()
    {
        plog!("H8 net: PANIC while starting (swallowed)");
        panel::step_lines(Step::Net, false, &["FAILED: the net probe panicked while starting".to_string(), "see the log for the panic message".to_string()]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    // --- mock-host.txt -----------------------------------------------------------------------------

    #[test]
    fn a_host_port_parses_with_the_trailing_newline_build_sh_writes() {
        assert_eq!(parse_mock_host("10.0.2.2:32499\n").as_deref(), Ok("10.0.2.2:32499"));
        assert_eq!(parse_mock_host("  192.168.1.20:80\r\n").as_deref(), Ok("192.168.1.20:80"));
        assert_eq!(parse_mock_host("my-pc.local:32499").as_deref(), Ok("my-pc.local:32499"));
        assert_eq!(parse_mock_host("255.255.255.255:65535").as_deref(), Ok("255.255.255.255:65535"));
    }

    #[test]
    fn malformed_hosts_are_refused_with_a_reason() {
        for bad in [
            "", "10.0.2.2", ":32499", "10.0.2.2:", "10.0.2.2:0", "10.0.2.2:65536", "10.0.2.2:abc", "10.0.2.256:80", "10.0.2:80",
            "10.0.2.2.2:80", "http://10.0.2.2:80", "10.0.2.2:80/identity", "-bad.example:80", "a..b:80", "[::1]:80", "host name:80",
            "1.2.3.4:80:90",
        ] {
            assert!(parse_mock_host(bad).is_err(), "{bad:?} must be refused");
        }
        assert!(parse_mock_host("10.0.2.256:80").unwrap_err().contains("IPv4"));
        assert!(parse_mock_host("10.0.2.2:0").unwrap_err().contains("port"));
    }

    #[test]
    fn the_file_is_read_from_res_and_absence_is_not_an_error() {
        let _g = crate::testutil::log_serial();
        let dir = std::env::temp_dir().join(format!("plx-net-probe-res-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let d = dir.to_str().unwrap();
        assert_eq!(read_mock_host(Some(d)), MockHost::Absent, "no file: built without --mock-host");
        assert_eq!(read_mock_host(None), MockHost::Absent);
        assert_eq!(read_mock_host(Some("")), MockHost::Absent);
        std::fs::write(dir.join(MOCK_FILE), "10.0.2.2:32499\n").unwrap();
        assert_eq!(read_mock_host(Some(d)), MockHost::Set("10.0.2.2:32499".into()));
        std::fs::write(dir.join(MOCK_FILE), "not a host\n").unwrap();
        assert_eq!(read_mock_host(Some(d)), MockHost::Bad("not a host".into()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- reading the answer ------------------------------------------------------------------------

    #[test]
    fn the_machine_identifier_is_found_in_the_mocks_json_and_in_a_real_servers_xml() {
        let json = r#"{"MediaContainer": {"size": 0, "machineIdentifier": "6f9c0a1e2b3d4c5e6f7a8b9c0d1e2f3a4b5c6d7e", "version": "1.41.0.0000-synthetic"}}"#;
        assert_eq!(machine_identifier(json).as_deref(), Some("6f9c0a1e2b3d4c5e6f7a8b9c0d1e2f3a4b5c6d7e"));
        assert_eq!(machine_identifier(r#"{"machineIdentifier":"abc"}"#).as_deref(), Some("abc"));
        let xml = r#"<MediaContainer size="0" machineIdentifier="abc123" version="1.40"/>"#;
        assert_eq!(machine_identifier(xml).as_deref(), Some("abc123"));
    }

    #[test]
    fn a_missing_empty_or_non_string_identifier_is_none() {
        for body in ["", "{}", r#"{"machineIdentifier": ""}"#, r#"{"machineIdentifier": 42}"#, r#"{"machineIdentifier": null}"#, "<html>404</html>"] {
            assert_eq!(machine_identifier(body), None, "{body:?}");
        }
        let long = format!(r#"{{"machineIdentifier": "{}"}}"#, "x".repeat(65));
        assert_eq!(machine_identifier(&long), None, "an identifier past 64 characters is not one");
    }

    #[test]
    fn curl_codes_have_words_and_an_unknown_one_is_a_number() {
        assert_eq!(curl_error_text(7), "curl error 7: failed to connect to host");
        assert_eq!(curl_error_text(6), "curl error 6: could not resolve host");
        assert_eq!(curl_error_text(28), "curl error 28: operation timed out");
        assert_eq!(curl_error_text(999), "curl error 999");
    }

    #[test]
    fn the_candidate_list_here_is_the_one_plx_net_binds() {
        let net = include_str!("../../../../rust-modules/net/src/net.rs");
        let list = format!("pub curl: [{}]", CANDIDATES.iter().map(|c| format!("{c:?}")).collect::<Vec<_>>().join(", "));
        assert!(net.contains(&list), "net.rs no longer binds {list}; update CANDIDATES");
    }

    // --- the panel lines ---------------------------------------------------------------------------

    fn loaded() -> CurlFacts {
        CurlFacts { loaded: Ok("libcurl.so.4"), init_ok: true, version: Some("libcurl/7.53.1 OpenSSL/1.0.2k zlib/1.2.8".into()) }
    }

    fn report(curl: CurlFacts, mock: MockHost, get: Option<GetResult>, pending: bool) -> NetReport {
        NetReport { curl, mock, get, pending }
    }

    fn reply(status: u16, body: &str) -> Option<GetResult> {
        Some(GetResult::Reply { status, body: body.as_bytes().to_vec() })
    }

    #[test]
    fn a_good_run_is_green_and_names_the_library_the_version_the_status_and_the_identifier() {
        let r = report(loaded(), MockHost::Set("10.0.2.2:32499".into()), reply(200, r#"{"MediaContainer":{"machineIdentifier":"abc123"}}"#), false);
        let (ok, l) = r.lines();
        assert!(ok);
        assert_eq!(l[0], "OK: GET http://10.0.2.2:32499/identity -> 200, machineIdentifier abc123");
        assert_eq!(l[1], "libcurl libcurl.so.4: libcurl/7.53.1 OpenSSL/1.0.2k zlib/1.2.8");
    }

    #[test]
    fn a_failed_get_shows_the_curl_error_text_and_stays_on_the_verdict_line() {
        let r = report(loaded(), MockHost::Set("10.0.2.2:32499".into()), Some(GetResult::Failed { rc: Some(7), timed_out: false }), false);
        let (ok, l) = r.lines();
        assert!(!ok);
        assert_eq!(l[0], "FAILED: GET http://10.0.2.2:32499/identity: curl error 7: failed to connect to host");
        let timed = report(loaded(), MockHost::Set("10.0.2.2:1".into()), Some(GetResult::Failed { rc: None, timed_out: true }), false).lines().1;
        assert!(timed[0].ends_with("timed out"), "{timed:?}");
        let refused = report(loaded(), MockHost::Set("10.0.2.2:1".into()), Some(GetResult::Failed { rc: None, timed_out: false }), false).lines().1;
        assert!(refused[0].ends_with("refused before libcurl ran"), "{refused:?}");
    }

    #[test]
    fn a_status_that_is_not_200_or_an_answer_without_an_identifier_is_not_ok() {
        let h = MockHost::Set("10.0.2.2:32499".into());
        let (ok, l) = report(loaded(), h.clone(), reply(404, ""), false).lines();
        assert!(!ok && l[0].starts_with("NOT OK: GET http://10.0.2.2:32499/identity -> HTTP 404"), "{l:?}");
        let (ok, l) = report(loaded(), h, reply(200, "<html>hello</html>"), false).lines();
        assert!(!ok && l[0].contains("-> 200 but no machineIdentifier in 18 bytes"), "{l:?}");
    }

    #[test]
    fn libcurl_that_did_not_load_is_a_failure_that_names_every_name_tried() {
        let curl = CurlFacts { loaded: Err(format!("none of {} opened", CANDIDATES.join(" "))), init_ok: false, version: None };
        let (ok, l) = report(curl, MockHost::Set("10.0.2.2:32499".into()), None, false).lines();
        assert!(!ok);
        assert_eq!(l[0], "FAILED: libcurl did not load, so no request was made");
        assert!(l[1].contains("libcurl.so.4 libcurl.so.5 libcurl.4.dylib"), "{l:?}");
        let curl = CurlFacts { loaded: Err("libcurl.so.4 opened but lacks 2 required symbol(s)".into()), init_ok: false, version: None };
        assert!(report(curl, MockHost::Absent, None, false).lines().1[1].contains("lacks 2"));
    }

    #[test]
    fn a_loaded_libcurl_with_no_mock_host_is_green_and_says_no_request_was_made() {
        let (ok, l) = report(loaded(), MockHost::Absent, None, false).lines();
        assert!(ok);
        assert!(l[0].starts_with("libcurl LOADED; no GET made") && l[0].contains("--mock-host"), "{l:?}");
        assert!(l[1].starts_with("libcurl libcurl.so.4: libcurl/7.53.1"));
    }

    #[test]
    fn a_bad_mock_host_file_and_a_failed_global_init_are_failures() {
        let (ok, l) = report(loaded(), MockHost::Bad("http://x".into()), None, false).lines();
        assert!(!ok && l[0].contains("res/mock-host.txt is not HOST:PORT: http://x"), "{l:?}");
        let curl = CurlFacts { init_ok: false, ..loaded() };
        let (ok, l) = report(curl, MockHost::Set("10.0.2.2:1".into()), None, false).lines();
        assert!(!ok && l[0].contains("curl_global_init"), "{l:?}");
    }

    #[test]
    fn the_request_in_progress_is_a_green_provisional_line() {
        let (ok, l) = report(loaded(), MockHost::Set("10.0.2.2:32499".into()), None, true).lines();
        assert!(ok);
        assert_eq!(l[0], "libcurl LOADED; GET http://10.0.2.2:32499/identity in progress");
    }

    #[test]
    fn every_line_stays_inside_the_cap_even_for_a_huge_version_string_and_host() {
        let curl = CurlFacts { version: Some(format!("libcurl/8.5.0 {}", "OpenSSL/3.0.13 ".repeat(40))), ..loaded() };
        let host = format!("{}.example:32499", "a".repeat(60));
        for r in [
            report(curl.clone(), MockHost::Set(host.clone()), reply(200, r#"{"machineIdentifier":"x"}"#), false),
            report(curl.clone(), MockHost::Set(host.clone()), Some(GetResult::Failed { rc: Some(60), timed_out: false }), false),
            report(curl, MockHost::Bad("y".repeat(300)), None, false),
        ] {
            let (_, l) = r.lines();
            assert!(l.iter().all(|s| !s.is_empty() && s.chars().count() <= LINE_MAX), "{l:?}");
        }
    }

    // --- the worker --------------------------------------------------------------------------------

    fn ok_get(url: &str) -> GetResult {
        assert_eq!(url, "http://10.0.2.2:32499/identity");
        GetResult::Reply { status: 200, body: br#"{"machineIdentifier":"fake-id"}"#.to_vec() }
    }

    fn panicking_get(_url: &str) -> GetResult {
        panic!("the request blew up");
    }

    #[test]
    fn the_worker_reports_the_reply_and_a_panic_in_the_request_is_a_failure_not_a_crash() {
        let _g = crate::testutil::log_serial();
        let h = run(loaded(), MockHost::Set("10.0.2.2:32499".into()), ok_get).expect("a worker was started");
        let done = h.join().unwrap();
        assert_eq!(done.get, reply(200, r#"{"machineIdentifier":"fake-id"}"#));
        assert!(!done.pending && done.lines().0);

        let h = run(loaded(), MockHost::Set("10.0.2.2:32499".into()), panicking_get).unwrap();
        let done = h.join().expect("the worker thread itself did not panic");
        assert_eq!(done.get, Some(GetResult::Failed { rc: None, timed_out: false }));
        assert!(!done.lines().0);
    }

    #[test]
    fn no_worker_is_started_without_a_loaded_libcurl_or_without_a_mock_host() {
        let _g = crate::testutil::log_serial();
        assert!(run(loaded(), MockHost::Absent, ok_get).is_none());
        assert!(run(loaded(), MockHost::Bad("x".into()), ok_get).is_none());
        let none = CurlFacts { loaded: Err("none opened".into()), init_ok: false, version: None };
        assert!(run(none, MockHost::Set("10.0.2.2:32499".into()), ok_get).is_none());
    }

    /// The real path end to end on a development machine: libcurl (the Mac's own `libcurl.4.dylib`, or the
    /// Linux box's `libcurl.so.4`) through the app's own request function, to a one-shot loopback server
    /// that answers like the mock's `/identity`. Skipped, loudly, where no libcurl can be bound.
    #[test]
    fn the_real_get_reaches_a_loopback_server_through_the_apps_own_curl_path() {
        let _g = crate::testutil::log_serial();
        let facts = load_curl();
        if facts.loaded.is_err() || !facts.init_ok {
            eprintln!("SKIPPED: no libcurl can be bound on this machine ({:?})", facts.loaded);
            return;
        }
        assert!(facts.version.as_deref().is_some_and(|v| v.starts_with("libcurl/")), "{:?}", facts.version);
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut req = [0u8; 2048];
            let n = s.read(&mut req).unwrap();
            let body = r#"{"MediaContainer":{"size":0,"machineIdentifier":"loopback-id-1","version":"1.41.0.0000-synthetic"}}"#;
            write!(s, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            String::from_utf8_lossy(&req[..n]).into_owned()
        });
        let r = real_get(&format!("http://127.0.0.1:{port}/identity"));
        let request = server.join().unwrap();
        assert!(request.starts_with("GET /identity HTTP/1.1\r\n"), "{request:?}");
        let GetResult::Reply { status, body } = &r else { panic!("expected a reply, got {r:?}") };
        assert_eq!(*status, 200);
        assert_eq!(machine_identifier(&String::from_utf8_lossy(body)).as_deref(), Some("loopback-id-1"));
        let (ok, l) = report(facts, MockHost::Set(format!("127.0.0.1:{port}")), Some(r), false).lines();
        assert!(ok && l[0].ends_with("-> 200, machineIdentifier loopback-id-1"), "{l:?}");
    }

    #[test]
    fn the_real_get_to_a_closed_port_fails_with_curl_error_7_and_does_not_panic() {
        let _g = crate::testutil::log_serial();
        let facts = load_curl();
        if facts.loaded.is_err() || !facts.init_ok {
            eprintln!("SKIPPED: no libcurl can be bound on this machine ({:?})", facts.loaded);
            return;
        }
        // Bind then drop: the port is closed by the time curl connects.
        let port = TcpListener::bind(("127.0.0.1", 0)).unwrap().local_addr().unwrap().port();
        let r = real_get(&format!("http://127.0.0.1:{port}/identity"));
        assert_eq!(r, GetResult::Failed { rc: Some(7), timed_out: false });
        let (ok, l) = report(facts, MockHost::Set(format!("127.0.0.1:{port}")), Some(r), false).lines();
        assert!(!ok && l[0].ends_with("curl error 7: failed to connect to host"), "{l:?}");
    }
}
