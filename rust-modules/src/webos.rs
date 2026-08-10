//! Which webOS this television actually is.
//!
//! # Why the app needs to know, when it never did before
//!
//! Until now the only thing it asked was "does `libAcbAPI` exist" (`starfish.c`'s `vp_mode`), which
//! splits the world at exactly webOS 5.0 and nowhere else. That was enough while 4.x was the only
//! target. It is not enough now: Kodi — which plays video on 5, 6 and 10 — carries gates at
//! `>= 6` (audio re-setup) and `< 11` / `>= 11` (a seek fallback, a changed signature), and with a
//! single boolean those are literally inexpressible here.
//!
//! The more immediate value is smaller and worth more: **a bug report from hardware nobody here
//! owns currently cannot say which firmware it came from.** The webOS 6/10 playback failure was
//! reported as one thing and is probably two, and the logs could not tell them apart.
//!
//! # Where it comes from
//!
//! `/var/run/nyx/os_info.json`, read once, at boot. Kodi asks
//! `luna://com.webos.service.config/getConfigs` for `tv.nyx.platformCode`; this is the same
//! information from the file nyx writes, and it needs no LS2 client, no subscription and no
//! thread. Verified present on the dev set (webOS 4.5), which reports:
//!
//! ```text
//! "webos_release": "4.10.2",  "webos_release_codename": "goldilocks2-grampians"
//! ```
//!
//! The CODENAME is the more useful half and is why this reads the file rather than a version
//! service: webosbrew's own compatibility data buckets firmware by codename, one library set per
//! bucket (their `library-version` guide — `goldilocks` is 4.0~4.4, `goldilocks2` 4.5~4.10). So
//! logging it says which of THEIR buckets a report belongs to, not just a number.
//!
//! Parsed by hand rather than through a JSON crate: this is a flat object of string values written
//! by the platform, the crate has no JSON dependency, and a parser that cannot fail is the right
//! shape for something that must never keep the app from booting.
use std::sync::OnceLock;

const OS_INFO: &str = "/var/run/nyx/os_info.json";

/// What the set said about itself. Owned strings rather than borrows into the file, because the
/// file is read once and dropped; `OnceLock` because this is written exactly once, at boot, and
/// read from the render thread every frame the diagnostics panel is up.
#[derive(Debug, Default, Clone)]
pub(crate) struct Info {
    /// e.g. "4.10.2" — empty when unknown
    pub release: String,
    /// e.g. "goldilocks2-grampians" — webosbrew buckets firmware by this
    pub codename: String,
    /// e.g. "4.1.0"
    pub api: String,
    /// e.g. "webOS TV"
    pub name: String,
    /// leading component of `release`, or 0 when unknown
    pub major: u32,
}

static INFO: OnceLock<Info> = OnceLock::new();

/// What the set reported. All-empty with `major == 0` when the file could not be read — which is
/// the honest answer and is what the panel prints.
pub(crate) fn info() -> &'static Info {
    INFO.get_or_init(Info::default)
}


/// Pull `"key": "value"` out of a flat JSON object. Returns None rather than erroring: nothing
/// here is worth failing a boot over.
fn field<'a>(s: &'a str, key: &str) -> Option<&'a str> {
    let at = s.find(&format!("\"{key}\""))?;
    let rest = &s[at + key.len() + 2..];
    let colon = rest.find(':')?;
    let open = rest[colon..].find('"')? + colon + 1;
    let close = rest[open..].find('"')? + open;
    Some(&rest[open..close])
}

/// Everything [`probe`] extracts, as a pure function of the file's text — so the parse is testable
/// without a filesystem, and an unreadable file and an unparseable one land on the same value.
fn parse(s: &str) -> Info {
    let get = |k: &str| field(s, k).unwrap_or_default().to_string();
    let release = get("webos_release");
    let major = release.split('.').next().and_then(|m| m.parse::<u32>().ok()).unwrap_or(0);
    Info {
        release,
        codename: get("webos_release_codename"),
        api: get("webos_api_version"),
        name: get("webos_name"),
        major,
    }
}

/// Read it once and log it. Called at boot; safe to call when the file does not exist.
pub(crate) fn probe() {
    let info = match std::fs::read_to_string(OS_INFO) {
        Ok(s) => parse(&s),
        Err(e) => {
            crate::log(&format!("webos: {OS_INFO} unreadable ({e}) — version unknown"));
            Info::default()
        }
    };
    if info.major > 0 {
        crate::log(&format!(
            "webos: {} release={} codename={} api={} major={}",
            info.name, info.release, info.codename, info.api, info.major
        ));
    }
    let _ = INFO.set(info);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real file off the dev set, verbatim. The parser has to survive the platform's
    /// formatting, not a tidied version of it.
    const REAL: &str = r#"{
    "core_os_kernel_version": "4.4.84-169.gld4tv.5",
    "core_os_name": "Rockhopper",
    "core_os_release": "4.10.2-31",
    "core_os_release_codename": "goldilocks2-grampians",
    "encryption_key_type": "prodkey",
    "webos_api_version": "4.1.0",
    "webos_build_datetime": "20250827000043",
    "webos_name": "webOS TV",
    "webos_prerelease": "",
    "webos_release": "4.10.2",
    "webos_release_codename": "goldilocks2-grampians"
}"#;

    #[test]
    fn reads_the_dev_sets_real_os_info() {
        assert_eq!(field(REAL, "webos_release"), Some("4.10.2"));
        assert_eq!(field(REAL, "webos_release_codename"), Some("goldilocks2-grampians"));
        assert_eq!(field(REAL, "webos_api_version"), Some("4.1.0"));
        assert_eq!(field(REAL, "webos_name"), Some("webOS TV"));
    }

    /// `webos_release` must not be satisfied by `core_os_release`, which appears FIRST in the file
    /// and whose value ("4.10.2-31") differs. A substring search that ignored the quotes would
    /// return the wrong field on every real device.
    #[test]
    fn does_not_match_a_longer_key_that_contains_it() {
        assert_ne!(field(REAL, "webos_release"), field(REAL, "core_os_release"));
        assert_eq!(field(REAL, "core_os_release"), Some("4.10.2-31"));
    }

    /// An empty value is a value, not a miss — `webos_prerelease` is empty on a shipping set.
    #[test]
    fn an_empty_string_value_parses_as_empty() {
        assert_eq!(field(REAL, "webos_prerelease"), Some(""));
    }

    /// Garbage in must not panic: this runs during boot, before anything is on screen.
    #[test]
    fn malformed_input_is_none_not_a_panic() {
        for bad in ["", "{", "{\"webos_release\"", "{\"webos_release\":", "not json at all"] {
            assert_eq!(field(bad, "webos_release"), None, "input {bad:?}");
        }
    }

    /// Absent key, present file.
    #[test]
    fn a_missing_key_is_none() {
        assert_eq!(field(REAL, "no_such_key"), None);
    }

    #[test]
    fn parses_the_whole_record() {
        let i = parse(REAL);
        assert_eq!((i.release.as_str(), i.major), ("4.10.2", 4));
        assert_eq!(i.codename, "goldilocks2-grampians");
        assert_eq!(i.name, "webOS TV");
    }

    /// The unknown case must be EMPTY with major 0, never a plausible-looking default. A panel that
    /// invents "4.0" for a set it could not read is worse than one that admits it does not know.
    #[test]
    fn an_unreadable_file_yields_no_version_rather_than_a_guess() {
        let i = parse("not json at all");
        assert_eq!(i.major, 0);
        assert!(i.release.is_empty() && i.codename.is_empty());
    }

    /// A two-component release still yields its major — LG has shipped "6.0" as well as "4.10.2".
    #[test]
    fn a_short_release_still_gives_a_major() {
        assert_eq!(parse(r#"{"webos_release": "6.0"}"#).major, 6);
        assert_eq!(parse(r#"{"webos_release": "10.0.1"}"#).major, 10);
    }
}
