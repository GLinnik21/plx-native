//! **The decision, and the identities that only exist because of it.**
//!
//! Two independent switches — errors and usage — both off until somebody turns them on, plus ONE
//! random identifier PER SWITCH, each minted by that switch's opt-in and destroyed by that switch's
//! withdrawal. Everything in this file is either a pure transition over a [`Consent`] value or the
//! storage under it, so the compliance-critical half is host-tested and needs no television and no
//! vendor.
//!
//! # The three rules that shape it
//!
//! **Nothing is stored to enable telemetry before consent.** Only the decision itself and the
//! policy version it was given against. In particular no identifier: [`apply`] mints one on the
//! transition into a channel's consent, never for the other channel, never at boot or "just in
//! case", and [`no_identifier_exists_before_anyone_says_yes`] is the test that keeps it that way.
//!
//! **Two identifiers, because they are two channels.** The crash-report id (`errors_id`) exists so
//! that Sentry can count how many opted-in televisions an issue reached rather than how many times
//! it fired — its built-in "users affected" reads exactly `user.id` and nothing else. The
//! analytics id (`install_id`) is PostHog's `distinct_id`. They are never the same value and never
//! travel together: a person who consented to two purposes did not consent to having them joined,
//! and one shared handle is precisely the join. Withdrawing one channel destroys ITS id and leaves
//! the other untouched.
//!
//! **Two switches, because they are two questions.** Error reports and usage statistics are judged
//! differently by the people who care — when Audacity retreated it dropped usage analytics and kept
//! error reporting — and bundling them into one "analytics?" toggle is the shape that reads as a
//! trick. Two `bool`s, both defaulting to false, and consenting to one says nothing about the other.
//!
//! **Withdrawal DELETES the identifier**, and that is a change from the plan this was built to,
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
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::RwLock;

/// The version of *what is collected and why*. Bumping it re-asks.
///
/// **This is not the wire schema's version**, and the distinction collapses the moment nobody
/// writes the rule down, so: **a new field bumps this unless it is derivable from a field already
/// declared.** A raster added beside an existing width and height does not; a new event does. The
/// incentive gradient runs towards never bumping — every bump costs consent — which is exactly why
/// the rule lives here rather than in a reviewer's head.
// Version 4 combines the compatibility/network dimensions introduced by version 3 with handled
// playback-error events and their bounded typed breadcrumb sequence. Existing version-3 answers
// covered the former but not the latter, so they must be asked again rather than silently expanded.
//
// The crash-report identifier (`errors_id`, 2026-09-04) is a new collected field and did NOT bump
// this, by decision: no shipped build has ever carried telemetry (v0.5.0 predates the module), so
// there is no version-4 answer in the world to expand — only the maintainer's own debug installs,
// which are re-answered by hand. The first release that ships the question ships it with the
// identifier already in it. The rule above stands for every bump after that one.
pub(crate) const POLICY_VERSION: u32 = 4;

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
    /// The ANALYTICS id: 16 random bytes as lowercase hex, minted when usage analytics is enabled
    /// and dropped when usage analytics is withdrawn. PostHog's `distinct_id`, and nothing else's.
    /// **Never derived from anything**: not the serial, not the MAC, not LG's `LGUDID`,
    /// not the Plex account id, not `X-Plex-Client-Identifier`, not the server's
    /// `machineIdentifier`. A derived identifier would survive this file being deleted, which is
    /// the property that makes it an identifier rather than a preference.
    ///
    /// The name predates the second identifier below and is kept because it is the stored key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install_id: Option<String>,
    /// The CRASH-REPORT id: same shape and same source, minted when crash reports are enabled and
    /// dropped when they are withdrawn. Sent as Sentry's `user.id` on every report of that
    /// channel — the native envelope, both fallback shapes and the handled playback error — and
    /// never to PostHog. It is what makes "users affected" a count of televisions instead of a
    /// count of events. Independent of [`Self::install_id`] in both directions: minted, kept and
    /// destroyed by its own switch alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub errors_id: Option<String>,
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
/// a test can prove the mint was never reached. It is called at most once PER CHANNEL, and the two
/// channels never share a result.
///
/// **A channel whose mint fails is recorded as OFF.** `None` from `mint` means there was no
/// randomness to draw on, and an opt-in with no identifier is not something this design can
/// honour: inventing one from a clock or a MAC is exactly the derived identifier the field docs
/// refuse, and sending reports with no identifier would make the channel silently mean something
/// different on that one television. The other channel is unaffected — each is judged on its own
/// mint. `/dev/urandom` does not fail on this platform; the branch exists so that the behaviour is
/// a decision rather than an accident.
///
/// Five behaviours, and each is a test below:
/// * enabling a channel, with no identifier yet, mints one for THAT channel;
/// * enabling it again does NOT re-mint;
/// * disabling a channel DROPS its identifier, independently of the other channel;
/// * a channel whose mint returns `None` is recorded as off, and the other channel still counts;
/// * the answer is recorded against the current [`POLICY_VERSION`] either way, so a "no" is a real
///   answer and is not re-asked until the policy itself changes.
pub(crate) fn apply(
    prev: &Consent,
    errors: bool,
    usage: bool,
    mut mint: impl FnMut() -> Option<String>,
) -> Consent {
    let mut keep_or_mint = |on: bool, prev_id: &Option<String>| -> Option<String> {
        if !on {
            return None;
        }
        match prev_id {
            Some(id) if !id.is_empty() => Some(id.clone()),
            _ => mint().filter(|id| !id.is_empty()),
        }
    };
    let errors_id = keep_or_mint(errors, &prev.errors_id);
    let install_id = keep_or_mint(usage, &prev.install_id);
    Consent {
        asked_version: POLICY_VERSION,
        errors: errors && errors_id.is_some(),
        usage: usage && install_id.is_some(),
        install_id,
        errors_id,
    }
}

// ---- the cached snapshot, and the disk under it ------------------------------------------------

/// What [`allows_usage`] reads. Published by [`install`]; never a disk read on the event path.
static CURRENT: RwLock<Option<Consent>> = RwLock::new(None);
/// Monotone process-local decision revision. A sender captures it before reading the spool and
/// abandons that batch if *any* decision changes, so records from an old opt-in cannot become
/// eligible again after a quick off→on cycle. It is never stored or sent.
static REVISION: AtomicU32 = AtomicU32::new(0);

/// Make `c` the decision every later [`allows_usage`] sees. Called after a load or a save.
pub(crate) fn install(c: Consent) {
    if let Ok(mut g) = CURRENT.write() {
        *g = Some(c);
        REVISION.fetch_add(1, Ordering::SeqCst);
    }
}

pub(crate) fn revision() -> u32 {
    REVISION.load(Ordering::SeqCst)
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
/// the same reason. It gates both `crashreport::report_pending` (the only thing that opens the
/// crash log at all) and the sparse in-memory playback-error trace. Consent gates collection, not
/// just the send: a television whose owner said no is neither scanned for faults nor traced.
pub(crate) fn allows_errors() -> bool {
    CURRENT
        .read()
        .map(|g| g.as_ref().is_some_and(|c| c.answered() && c.errors))
        .unwrap_or(false)
}

/// The crash-report identifier every Sentry-bound report carries, or `None` when the channel is
/// off — read from the snapshot, like the two gates, so a producer on the render thread never
/// touches the disk for it. `None` while [`allows_errors`] is true cannot happen through [`apply`],
/// but a producer must READ it rather than assume it: the failure would be a report carrying a
/// fabricated or empty id, which is the one outcome this field exists to make impossible.
pub(crate) fn errors_id() -> Option<String> {
    CURRENT.read().ok().and_then(|g| {
        g.as_ref()
            .filter(|c| c.answered() && c.errors)
            .and_then(|c| c.errors_id.clone())
    })
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

    /// **Nothing is stored to enable telemetry before consent** — both identifiers included.
    /// Proven by the mint never being reached, not by inspecting the result: a test that only
    /// checked `is_none()` would pass a version that minted one and threw it away.
    #[test]
    fn no_identifier_exists_before_anyone_says_yes() {
        let fresh = Consent::default();
        assert!(fresh.install_id.is_none() && fresh.errors_id.is_none());
        let after_no = apply(&fresh, false, false, || {
            panic!("minted an identifier for a refusal")
        });
        assert!(after_no.install_id.is_none() && after_no.errors_id.is_none());
        assert!(
            after_no.answered(),
            "a refusal IS an answer and is not re-asked"
        );
        assert!(!should_ask(&after_no, false));
    }

    /// A counting mint: hands out `m1`, `m2`, … so a test can see HOW MANY identifiers were drawn
    /// and which channel each landed in.
    fn counting_mint() -> impl FnMut() -> Option<String> {
        let mut n = 0;
        move || {
            n += 1;
            Some(format!("m{n}"))
        }
    }

    /// Each channel's first yes mints exactly one identifier, for that channel alone.
    #[test]
    fn the_first_yes_mints_one_identifier_per_channel() {
        let usage = apply(&Consent::default(), false, true, counting_mint());
        assert_eq!(usage.install_id.as_deref(), Some("m1"));
        assert!(
            usage.errors_id.is_none(),
            "usage-only minted a crash-report id"
        );

        let errors = apply(&Consent::default(), true, false, counting_mint());
        assert_eq!(errors.errors_id.as_deref(), Some("m1"));
        assert!(
            errors.install_id.is_none(),
            "errors-only minted an analytics id"
        );

        let both = apply(&Consent::default(), true, true, counting_mint());
        assert!(both.errors && both.usage);
        assert_ne!(
            both.errors_id, both.install_id,
            "the two channels were handed ONE identifier — that is the join the design refuses"
        );
        assert!(both.errors_id.is_some() && both.install_id.is_some());
    }

    /// Errors-only consent draws no usage identifier: PostHog's `distinct_id` must not exist on a
    /// television whose owner never said yes to product analytics.
    #[test]
    fn errors_only_never_mints_a_usage_identifier() {
        let c = apply(&Consent::default(), true, false, counting_mint());
        assert!(c.errors && !c.usage && c.install_id.is_none());
        assert_eq!(
            c.errors_id.as_deref(),
            Some("m1"),
            "exactly one draw, for errors"
        );
    }

    /// …and a SECOND yes does not re-mint. Turning the other switch on later is the same install
    /// changing its mind, not a new one — re-minting there would silently split one person's
    /// reports in two and make every count wrong.
    #[test]
    fn a_second_yes_keeps_the_identifier_it_already_had() {
        let first = apply(&Consent::default(), false, true, || Some("abc123".into()));
        let second = apply(&first, true, true, || Some("def456".into()));
        assert_eq!(
            second.install_id.as_deref(),
            Some("abc123"),
            "re-minted the analytics id"
        );
        assert_eq!(second.errors_id.as_deref(), Some("def456"));
        let third = apply(&second, true, true, || {
            panic!("re-minted on an unchanged answer")
        });
        assert_eq!(third, second);
    }

    /// **Withdrawal drops the identifier.** Neither vendor can delete data belonging to no account,
    /// so severing the link locally is the only thing this app controls: a later opt-in is a new
    /// install rather than a resumed profile.
    #[test]
    fn withdrawing_everything_destroys_both_identifiers() {
        let on = apply(&Consent::default(), true, true, counting_mint());
        let off = apply(&on, false, false, || panic!("minted on a withdrawal"));
        assert!(
            off.install_id.is_none() && off.errors_id.is_none(),
            "an identifier survived the withdrawal"
        );
        assert!(off.answered(), "and it is still an answered question");

        // …and coming back later is a genuinely fresh identity, not the old one resumed.
        let again = apply(&off, true, true, || Some("fresh".into()));
        assert_eq!(again.install_id.as_deref(), Some("fresh"));
        assert_eq!(again.errors_id.as_deref(), Some("fresh"));
    }

    /// Withdrawing usage destroys its identity even while independent crash consent remains on —
    /// and leaves the crash-report id exactly where it was.
    #[test]
    fn withdrawing_usage_while_errors_remain_destroys_only_the_usage_identifier() {
        let both = apply(&Consent::default(), true, true, counting_mint());
        let errors = apply(&both, true, false, || panic!("re-minted on a withdrawal"));
        assert!(errors.errors && !errors.usage);
        assert!(errors.install_id.is_none());
        assert_eq!(
            errors.errors_id, both.errors_id,
            "the crash-report id was disturbed"
        );
        let again = apply(&errors, true, true, || Some("def456".into()));
        assert_eq!(again.install_id.as_deref(), Some("def456"));
        assert_eq!(again.errors_id, both.errors_id);
    }

    /// The mirror image: withdrawing crash reports destroys the crash-report id and leaves the
    /// analytics id alone.
    #[test]
    fn withdrawing_errors_while_usage_remains_destroys_only_the_errors_identifier() {
        let both = apply(&Consent::default(), true, true, counting_mint());
        let usage = apply(&both, false, true, || panic!("re-minted on a withdrawal"));
        assert!(!usage.errors && usage.usage);
        assert!(usage.errors_id.is_none());
        assert_eq!(usage.install_id, both.install_id);
        let again = apply(&usage, true, true, || Some("def456".into()));
        assert_eq!(again.errors_id.as_deref(), Some("def456"));
        assert_eq!(again.install_id, both.install_id);
    }

    /// **A channel whose mint fails is off, and the other channel still counts.** The answer is
    /// still recorded, so the person is not asked again for a decision they made.
    #[test]
    fn a_failed_mint_refuses_only_the_channel_it_failed_for() {
        let neither = apply(&Consent::default(), true, true, || None);
        assert!(neither.answered());
        assert!(!neither.errors && !neither.usage);
        assert!(neither.errors_id.is_none() && neither.install_id.is_none());

        // The FIRST draw is the crash-report id; failing only the second refuses only usage.
        let mut draws = 0;
        let errors_only = apply(&Consent::default(), true, true, || {
            draws += 1;
            (draws == 1).then(|| "e".repeat(32))
        });
        assert!(errors_only.errors && !errors_only.usage);
        assert!(errors_only.errors_id.is_some() && errors_only.install_id.is_none());

        // An empty string is not an identifier either.
        let empty = apply(&Consent::default(), true, false, || Some(String::new()));
        assert!(!empty.errors && empty.errors_id.is_none());
    }

    /// The crash-report id accessor reads the SNAPSHOT and fails closed exactly like the gates: a
    /// stale-policy yes, or an unanswered decision, yields no id however the field is set.
    #[test]
    fn the_errors_id_accessor_fails_closed_with_the_gate() {
        let _g = crate::testlock::serial();
        let saved = CURRENT.read().ok().and_then(|g| g.clone());
        install(Consent {
            asked_version: POLICY_VERSION - 1,
            errors: true,
            errors_id: Some("stale".into()),
            ..Default::default()
        });
        assert!(!allows_errors());
        assert!(
            errors_id().is_none(),
            "a stale-policy id must not be reported"
        );
        install(apply(&Consent::default(), true, false, || {
            Some("live".into())
        }));
        assert_eq!(errors_id().as_deref(), Some("live"));
        install(apply(&Consent::default(), false, true, || {
            Some("usage".into())
        }));
        assert!(
            errors_id().is_none(),
            "the analytics id is not the crash-report id"
        );
        if let Ok(mut g) = CURRENT.write() {
            *g = saved;
        }
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

    #[test]
    fn every_published_decision_invalidates_an_in_flight_sender_batch() {
        let _g = crate::testlock::serial();
        let saved = CURRENT.read().ok().and_then(|g| g.clone());
        let before = revision();
        install(Consent {
            asked_version: POLICY_VERSION,
            errors: true,
            ..Default::default()
        });
        assert_ne!(
            revision(),
            before,
            "the sender would keep using its stale decision"
        );
        if let Ok(mut g) = CURRENT.write() {
            *g = saved;
        }
    }
}
