//! **The decision, and the identity that only exists because of it.**
//!
//! Two independent switches — errors and usage — both off until somebody turns them on, plus the
//! random identifier that is minted by usage opt-in and destroyed by usage withdrawal.
//! Everything in this file is either a pure transition over a [`Consent`] value or the storage
//! under it, so the compliance-critical half is host-tested and needs no television and no vendor.
//!
//! # The three rules that shape it
//!
//! **Nothing is stored to enable telemetry before consent.** Only the decision itself and the
//! policy version it was given against. In particular no usage identifier: [`apply`] mints one on
//! the transition into usage consent, never for errors-only consent, never at boot or "just in case", and
//! [`no_identifier_exists_before_anyone_says_yes`] is the test that keeps it that way.
//!
//! **Two switches, because they are two questions.** Crash reports and usage statistics are judged
//! differently by the people who care — when Audacity retreated it dropped usage analytics and kept
//! error reporting — and bundling them into one "analytics?" toggle is the shape that reads as a
//! trick. Two `bool`s, both defaulting to false, and consenting to one says nothing about the other.
//!
//! **Usage withdrawal DELETES the identifier**, and that is a change from the plan this was built to,
//! forced by a measurement. The plan said keep the id, request deletion from the vendor, and rotate
//! only once that succeeded. Neither vendor can delete anonymous data belonging to no account
//! (`PRIVACY.md` term 7 records why), so "keep it pending a deletion" would be keeping it forever.
//! Dropping it locally is the one thing this app actually controls: it severs any future opt-in
//! from everything sent before, so a person who turns it off and later back on is a new install
//! rather than a resumed profile.
//!
//! # What must NOT happen on the event path
//!
//! [`allows_usage`] is called from `diag::event`, which is reached from the frame loop. It reads a
//! cached snapshot and never touches the disk or a lock. That is not an optimisation — it is the
//! bug this crate already shipped once: wiring the scrubber's identity list to `session::peek()`
//! put five file reads on every log line and deadlocked the whole `auth` test block. The fix there
//! and the design here are the same: the writer PUBLISHES, the hot path reads a snapshot.
//!
//! # The four `#[allow(dead_code)]`s are gone
//!
//! [`POLICY_VERSION`], [`should_ask`], [`apply`] and `telemetry::record` carried one between them,
//! each naming the consent SCREEN as the missing caller. `ui::consent` is that screen, and the
//! attributes were deleted by the commit that added it rather than left behind — which was the
//! stated plan and is worth having actually happened, because a stale allowance is how a genuinely
//! dead function later hides in plain sight.
use std::sync::RwLock;

/// The version of *what is collected and why*. Bumping it re-asks.
///
/// **This is not the wire schema's version**, and the distinction collapses the moment nobody
/// writes the rule down, so: **a new field bumps this unless it is derivable from a field already
/// declared.** A raster added beside an existing width and height does not; a new event does. The
/// incentive gradient runs towards never bumping — every bump costs consent — which is exactly why
/// the rule lives here rather than in a reviewer's head.
// Version 3 adds webOS/model/SoC/hardware compatibility dimensions to both channels and coarse
// local/remote/relay plus IPv4/IPv6 classes to usage. Existing answers were given against a schema
// without them, so they must be asked again rather than silently expanded.
pub(crate) const POLICY_VERSION: u32 = 3;

/// The stored decision. Serde-serialised to the telemetry file; every field is read and written, so
/// none of them is dead even while only one accessor has a caller.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct Consent {
    /// The [`POLICY_VERSION`] this decision was made against. `0` means never asked — which is
    /// what a fresh install has, and is deliberately distinguishable from "asked, and said no to
    /// everything".
    #[serde(default)]
    pub asked_version: u32,
    /// crash and error reports
    #[serde(default)]
    pub errors: bool,
    /// which screens and features get used
    #[serde(default)]
    pub usage: bool,
    /// 16 random bytes as lowercase hex, minted when usage analytics is enabled and dropped when
    /// usage analytics is withdrawn. **Never derived from anything**: not the serial, not the MAC,
    /// not LG's `LGUDID`,
    /// not the Plex account id, not `X-Plex-Client-Identifier`, not the server's
    /// `machineIdentifier`. A derived identifier would survive this file being deleted, which is
    /// the property that makes it an identifier rather than a preference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install_id: Option<String>,
}

impl Consent {
    /// Has this person been asked, against the CURRENT policy? A bump re-asks.
    pub(crate) fn answered(&self) -> bool {
        self.asked_version >= POLICY_VERSION
    }
    /// Is anything switched on at all?
    pub(crate) fn any(&self) -> bool {
        self.errors || self.usage
    }
}

/// Should the app put the consent question on screen?
///
/// `automated` is `dev::any_trigger_present()` at the call site. An automated boot must never land
/// on this screen: `tests/run.py` injects a token and expects Home, the fps scenes grade a
/// heartbeat on a known route, and every `sim-shot` script drives a screen it chose. Getting this
/// wrong would not fail loudly — it would quietly re-point every headless run at a screen nobody
/// wrote an assertion for.
pub(crate) fn should_ask(c: &Consent, automated: bool) -> bool {
    !automated && !c.answered()
}

/// **The one transition.** Apply a person's answer to the previous state.
///
/// `mint` is only called when an identifier is actually needed, which is what makes "nothing is
/// stored before consent" checkable rather than asserted: the randomness source is a parameter, so
/// a test can prove the mint was never reached.
///
/// Four behaviours, and each is a test below:
/// * enabling usage, with no identifier yet, mints one;
/// * enabling usage again does NOT re-mint;
/// * disabling usage DROPS the identifier, independently of crash consent;
/// * the answer is recorded against the current [`POLICY_VERSION`] either way, so a "no" is a real
///   answer and is not re-asked until the policy itself changes.
pub(crate) fn apply(
    prev: &Consent,
    errors: bool,
    usage: bool,
    mint: impl FnOnce() -> String,
) -> Consent {
    let next = Consent {
        asked_version: POLICY_VERSION,
        errors,
        usage,
        install_id: None,
    };
    Consent {
        install_id: match (&prev.install_id, usage) {
            (_, false) => None,
            (Some(id), true) => Some(id.clone()),
            (None, true) => Some(mint()),
        },
        ..next
    }
}

// ---- the cached snapshot, and the disk under it ------------------------------------------------

/// What [`allows_usage`] reads. Published by [`install`]; never a disk read on the event path.
static CURRENT: RwLock<Option<Consent>> = RwLock::new(None);

/// Make `c` the decision every later [`allows_usage`] sees. Called after a load or a save.
pub(crate) fn install(c: Consent) {
    if let Ok(mut g) = CURRENT.write() {
        *g = Some(c);
    }
}

/// The decision as last published, if one has been. `None` means nothing has been loaded yet —
/// distinct from "a decision that allows nothing", which is what a refusal looks like, and the
/// consent screen needs to tell those apart to seed itself honestly.
pub(crate) fn current() -> Option<Consent> {
    CURRENT.read().ok().and_then(|g| g.clone())
}

/// May a USAGE event be reported? Read from the snapshot, so this is safe to call per event.
///
/// **Fails closed.** No snapshot installed, or a poisoned lock, both answer `false` — a build that
/// has not loaded a decision has not been given one, and the only safe reading of "I do not know"
/// here is no.
pub(crate) fn allows_usage() -> bool {
    CURRENT
        .read()
        .map(|g| g.as_ref().is_some_and(|c| c.answered() && c.usage))
        .unwrap_or(false)
}

/// May an ERROR report be sent? The crash channel's twin of [`allows_usage`], failing closed for
/// the same reason and read at the same one place — `crashreport::report_pending`, which is the
/// only thing that opens the crash log at all. Consent gates the READ, not just the send: a
/// television whose owner said no is not scanned for faults.
pub(crate) fn allows_errors() -> bool {
    CURRENT
        .read()
        .map(|g| g.as_ref().is_some_and(|c| c.answered() && c.errors))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The default is OFF, for both, and unanswered — not "off because someone said no".
    #[test]
    fn nothing_is_on_until_somebody_says_so() {
        let c = Consent::default();
        assert!(!c.errors && !c.usage, "both switches start off");
        assert!(!c.any());
        assert!(!c.answered(), "and the question has not been asked yet");
        assert!(should_ask(&c, false));
    }

    /// **Nothing is stored to enable telemetry before consent** — the identifier included. Proven
    /// by the mint never being reached, not by inspecting the result: a test that only checked
    /// `install_id.is_none()` would pass a version that minted one and threw it away.
    #[test]
    fn no_identifier_exists_before_anyone_says_yes() {
        let fresh = Consent::default();
        assert!(fresh.install_id.is_none());
        let after_no = apply(&fresh, false, false, || {
            panic!("minted an identifier for a refusal")
        });
        assert!(after_no.install_id.is_none());
        assert!(
            after_no.answered(),
            "a refusal IS an answer and is not re-asked"
        );
        assert!(!should_ask(&after_no, false));
    }

    /// Only usage analytics needs a stable identifier. Crash reports use independent event ids.
    #[test]
    fn the_first_yes_mints_one_identifier() {
        for (errors, usage) in [(false, true), (true, true)] {
            let c = apply(&Consent::default(), errors, usage, || "abc123".into());
            assert_eq!(
                c.install_id.as_deref(),
                Some("abc123"),
                "errors={errors} usage={usage}"
            );
        }
    }

    #[test]
    fn errors_only_never_mints_a_usage_identifier() {
        let c = apply(&Consent::default(), true, false, || {
            panic!("minted a usage identifier for crash reporting")
        });
        assert!(c.errors && !c.usage && c.install_id.is_none());
    }

    /// …and a SECOND yes does not re-mint. Turning the other switch on later is the same install
    /// changing its mind, not a new one — re-minting there would silently split one person's
    /// reports in two and make every count wrong.
    #[test]
    fn a_second_yes_keeps_the_identifier_it_already_had() {
        let first = apply(&Consent::default(), false, true, || "abc123".into());
        let second = apply(&first, true, true, || {
            panic!("re-minted on a second opt-in")
        });
        assert_eq!(second.install_id.as_deref(), Some("abc123"));
    }

    /// **Withdrawal drops the identifier.** Neither vendor can delete data belonging to no account,
    /// so severing the link locally is the only thing this app controls: a later opt-in is a new
    /// install rather than a resumed profile.
    #[test]
    fn withdrawing_everything_destroys_the_identifier() {
        let on = apply(&Consent::default(), true, true, || "abc123".into());
        let off = apply(&on, false, false, || panic!("minted on a withdrawal"));
        assert!(
            off.install_id.is_none(),
            "the identifier did not survive the withdrawal"
        );
        assert!(off.answered(), "and it is still an answered question");

        // …and coming back later is a genuinely fresh identity, not the old one resumed.
        let again = apply(&off, false, true, || "def456".into());
        assert_eq!(again.install_id.as_deref(), Some("def456"));
    }

    /// Withdrawing usage destroys its identity even while independent crash consent remains on.
    #[test]
    fn withdrawing_usage_while_errors_remain_destroys_the_identifier() {
        let both = apply(&Consent::default(), true, true, || "abc123".into());
        let errors = apply(&both, true, false, || panic!("re-minted on a withdrawal"));
        assert!(errors.errors && !errors.usage);
        assert!(errors.install_id.is_none());
        let again = apply(&errors, true, true, || "def456".into());
        assert_eq!(again.install_id.as_deref(), Some("def456"));
    }

    /// A policy bump re-asks, and does NOT silently carry the old answer forward as consent.
    #[test]
    fn a_policy_bump_re_asks() {
        let old = Consent {
            asked_version: POLICY_VERSION - 1,
            usage: true,
            ..Default::default()
        };
        assert!(
            !old.answered(),
            "an answer to an older policy is not an answer to this one"
        );
        assert!(should_ask(&old, false));
        let _g = crate::testlock::serial();
        let saved = CURRENT.read().ok().and_then(|g| g.clone());
        install(old);
        assert!(
            !allows_usage(),
            "an old yes must not authorize fields added by the new policy"
        );
        if let Ok(mut g) = CURRENT.write() {
            *g = saved;
        }
    }

    /// **An automated boot never sees the question**, whatever the stored state. `tests/run.py`,
    /// the fps scenes and every `sim-shot` script drive a screen they chose; a consent prompt in
    /// front of it would not fail loudly, it would quietly re-point them all.
    #[test]
    fn an_automated_boot_is_never_asked() {
        assert!(!should_ask(&Consent::default(), true));
    }

    /// The event path fails CLOSED: with nothing installed, nothing is allowed.
    #[test]
    fn the_event_path_fails_closed() {
        let _g = crate::testlock::serial();
        let saved = CURRENT.read().ok().and_then(|g| g.clone());

        install(Consent::default());
        assert!(!allows_usage(), "a default decision allows nothing");
        install(Consent {
            asked_version: POLICY_VERSION,
            usage: true,
            ..Default::default()
        });
        assert!(allows_usage());
        install(Consent {
            asked_version: POLICY_VERSION,
            errors: true,
            ..Default::default()
        });
        assert!(
            !allows_usage(),
            "consenting to ERRORS does not consent to usage"
        );

        if let Ok(mut g) = CURRENT.write() {
            *g = saved;
        }
    }
}
