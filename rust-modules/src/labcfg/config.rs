//! `lab.json` — the per-session configuration, read once from the **app directory**.
//!
//! # Why the app directory and not `/tmp`
//!
//! Every other knob in this app arrives as a `/tmp/plxnative-*` trigger written over ssh, and on a
//! Cloud Test Lab set there is no ssh: the **.ipk is the only channel into the device**. So the
//! session's endpoint, secret and certificate pin are staged into the package beside the binary
//! (`make LAB=1 … ipk`, `ci/mkipk.py`) and resolved here through [`crate::paths::in_app_dir`] — the
//! same `/proc/self/exe` resolution everything else in this app uses, so it is correct under both
//! install prefixes and both jail profiles.
//!
//! One consequence worth stating: a session change is a **repack**, not a rebuild. The file is
//! data, nothing about it reaches codegen, and `pkg/lab.json` is gitignored and listed in
//! `.claude/hooks/outbound-guard.py`'s `PRIVATE_FILES` because it holds a live secret.
//!
//! # Why the trigger codes are configuration
//!
//! **The codes ARE known now** — BLUE is `wcode` **489** (486 RED / 487 GREEN / 488 YELLOW,
//! `sym` 0), measured on the dev set 2026-08-26 and recorded in `docs/remote-keys.md` §9. The
//! trigger is a LIST in this file anyway, for two reasons that outlived the measurement.
//!
//! **One: the offline answer was WRONG, not merely missing.** The fork's evdev→scancode table maps
//! `KEY_RED`/`KEY_YELLOW`/`KEY_BLUE` (evdev 398/400/401) to nothing at all, and `KEY_GREEN` (399)
//! to a scancode 504 that this remote never sends — so the one colour key that looked derivable
//! from a desk was derivable incorrectly. The real codes come from LG's private evdev range
//! (289–292), which no table could have singled out.
//!
//! **Two: that is one remote on one firmware.** A Cloud Test Lab set may spell the buttons
//! differently, or its virtual remote may not offer them at all — which is why the account-menu
//! row reaches the same upload with the D-pad alone. And the app logs every press's raw bytes
//! unconditionally, so the first successful upload by any route re-answers the question for
//! whatever set it came from. `docs/lab-diagnostics.md` §7 is the full account.
use serde::Deserialize;
use std::sync::OnceLock;

/// The file's name in the app directory. Not under `/tmp`, so `crate::dev`'s rules do not apply to
/// it and it cannot be armed by a co-resident process on a user's television — a build without the
/// feature never opens it whatever it contains.
const FILE: &str = "lab.json";

#[derive(Deserialize, Default, Debug, Clone)]
pub(crate) struct Config {
    /// `host:port`, as the receiver published it. Scheme is not carried: it is always https.
    #[serde(default)]
    pub endpoint: String,
    /// Short session id, echoed in the envelope and in the `X-Plx-Session` header so the receiver
    /// can reject a stale build from a previous session by name rather than only by secret.
    #[serde(default)]
    pub session: String,
    /// The bearer secret. 32 random bytes, base64url, generated per session. **Never logged.**
    #[serde(default)]
    pub secret: String,
    /// `sha256//<base64>` — the receiver certificate's SPKI pin, handed to
    /// `CURLOPT_PINNEDPUBLICKEY`. Public by nature; it is a hash of a public key.
    #[serde(default)]
    pub pin: String,
    /// Whether this package opens the authenticated long-poll command channel. Missing is false,
    /// so a `lab.json` produced before Lab Control existed remains upload-only.
    #[serde(default)]
    pub control: bool,
    /// Which `wcode`s fire an upload. Zeroes are dropped at load: `wcode == 0` is what an
    /// unmapped key arrives as, and a zero in this list would make every such press upload.
    #[serde(default)]
    pub trigger_wcodes: Vec<u32>,
    /// …and which `sym`s, for a key that arrives in the other field (§1 of `docs/remote-keys.md`
    /// — which of the two a code lands in is not predictable from the code).
    #[serde(default)]
    pub trigger_syms: Vec<u32>,
}

impl Config {
    /// Diagnostic upload endpoint.
    pub fn url(&self) -> String {
        format!("https://{}/v1/diag", self.endpoint)
    }

    /// Outbound long-poll endpoint for the optional lab command channel.
    pub fn control_url(&self) -> String {
        format!("https://{}/v1/control/poll", self.endpoint)
    }

    pub fn is_trigger(&self, sym: u32, wcode: u32) -> bool {
        (wcode != 0 && self.trigger_wcodes.contains(&wcode))
            || (sym != 0 && self.trigger_syms.contains(&sym))
    }
}

/// Everything a configuration must have before the feature will do anything. Returns the reason it
/// is unusable, so [`why_not`] can put it in the log rather than leaving a tester with silence.
///
/// Pure, so the whole gate is host-testable without a filesystem.
fn validate(c: Config) -> Result<Config, &'static str> {
    if c.endpoint.is_empty() {
        return Err("no endpoint");
    }
    if c.endpoint.contains('/') || c.endpoint.contains(char::is_whitespace) {
        return Err("endpoint must be host:port");
    }
    if c.secret.is_empty() {
        return Err("no secret");
    }
    if !c.pin.starts_with("sha256//") || c.pin.len() < "sha256//".len() + 40 {
        return Err("pin must be sha256//<base64>");
    }
    let mut c = c;
    c.trigger_wcodes.retain(|w| *w != 0);
    c.trigger_syms.retain(|s| *s != 0);
    Ok(c)
}

/// The parse, as a pure function of the file's text — an unreadable file and an unparseable one
/// land on the same value, and neither can keep the app from booting.
fn parse(s: &str) -> Result<Config, &'static str> {
    let c: Config = serde_json::from_str(s).map_err(|_| "lab.json is not valid JSON")?;
    validate(c)
}

static LOADED: OnceLock<Result<Config, &'static str>> = OnceLock::new();

fn load() -> &'static Result<Config, &'static str> {
    LOADED.get_or_init(|| {
        let p = crate::paths::in_app_dir(FILE);
        match std::fs::read_to_string(&p) {
            Ok(s) => parse(&s),
            Err(_) => Err("no lab.json beside the binary"),
        }
    })
}

/// The session, or `None` when this build has no usable one. Every entry point in the module goes
/// through this, so an unconfigured lab build is inert rather than half-armed.
pub(crate) fn get() -> Option<&'static Config> {
    load().as_ref().ok()
}

/// Why [`get`] answered `None`. One short phrase, for the boot log.
pub(crate) fn why_not() -> &'static str {
    load().as_ref().err().copied().unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"{"endpoint":"lab.plxnative.com:39443","session":"a1b2c3d4",
        "secret":"c2VjcmV0LXNlY3JldC1zZWNyZXQtc2VjcmV0","pin":"sha256//9F8kQb2ZC0mQ3xY1t6nX0oPq7RkS4uVwXyZaBcDeFgH=",
        "trigger_wcodes":[406,0],"trigger_syms":[]}"#;

    #[test]
    fn a_good_file_parses_and_builds_the_one_url() {
        let c = parse(GOOD).expect("parses");
        assert_eq!(c.url(), "https://lab.plxnative.com:39443/v1/diag");
        assert_eq!(
            c.control_url(),
            "https://lab.plxnative.com:39443/v1/control/poll"
        );
        assert_eq!(c.session, "a1b2c3d4");
        assert!(!c.control, "an old config is upload-only by default");
    }

    #[test]
    fn control_is_an_explicit_package_capability() {
        let c = parse(
            r#"{"endpoint":"h:1","secret":"s","control":true,
            "pin":"sha256//aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}"#,
        )
        .expect("parses");
        assert!(c.control);
    }

    /// A zero in the trigger list would fire on every unmapped key — `wcode == 0` is exactly what
    /// a key the fork has no scancode for arrives as.
    #[test]
    fn a_zero_trigger_code_is_dropped_not_honoured() {
        let c = parse(GOOD).expect("parses");
        assert_eq!(c.trigger_wcodes, vec![406]);
        assert!(c.is_trigger(0, 406));
        assert!(!c.is_trigger(0, 0), "an unmapped key must not upload");
    }

    /// Each field's absence is its own refusal, and each refusal is a phrase the boot log prints —
    /// silence in a rented lab hour is the failure this exists to prevent.
    #[test]
    fn every_missing_field_names_itself() {
        for (bad, want) in [
            (r#"{}"#, "no endpoint"),
            (
                r#"{"endpoint":"https://x/y","secret":"s","pin":"sha256//aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}"#,
                "endpoint must be host:port",
            ),
            (r#"{"endpoint":"h:1"}"#, "no secret"),
            (
                r#"{"endpoint":"h:1","secret":"s"}"#,
                "pin must be sha256//<base64>",
            ),
            (
                r#"{"endpoint":"h:1","secret":"s","pin":"sha256//short"}"#,
                "pin must be sha256//<base64>",
            ),
            ("not json at all", "lab.json is not valid JSON"),
        ] {
            assert_eq!(parse(bad).err(), Some(want), "{bad}");
        }
    }

    /// A press matches in EITHER field, because which of the two a remote code lands in is not
    /// predictable from the code (`docs/remote-keys.md` §1).
    #[test]
    fn either_field_can_carry_the_trigger() {
        let c = parse(
            r#"{"endpoint":"h:1","secret":"s",
            "pin":"sha256//aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","trigger_syms":[504]}"#,
        )
        .expect("parses");
        assert!(c.is_trigger(504, 0));
        assert!(!c.is_trigger(0, 504), "syms are not wcodes");
    }
}
