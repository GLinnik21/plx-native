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
//! that Sentry can count how many uninterrupted opt-ins an issue reached rather than how many
//! times it fired — its built-in "users affected" reads exactly `user.id` and nothing else. The
//! analytics id (`install_id`) is PostHog's `distinct_id`. They are never the same value and never
//! travel together: a person who consented to two purposes did not consent to having them joined,
//! and one shared handle is precisely the join. Withdrawing one channel destroys ITS id and leaves
//! the other untouched. **Signing out destroys both and the decision with them**
//! (`telemetry::forget`, behind `auth::forget_account`): consent belongs to the account that gave
//! it, and the next account to sign in is asked afresh.
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
/// writes the rule down. **The rule is the PURPOSE, not the field count** (owner decision,
/// 2026-09-10 — scope, not policy version), and it has exactly two outcomes, which is why the
/// gradient argument below is not the whole story:
///
///  * A new field that **widens what is collected beyond the described purpose of the category it
///    rides in** bumps that category's SCOPE ([`ERRORS_SCOPE`]/[`USAGE_SCOPE`], with a
///    [`SCOPE_CHANGES`] row), and the people who have that category on are asked about that
///    category alone. A whole new report, or a new event, is always this.
///  * A new field **inside a purpose the notice already describes** bumps [`NOTICE_REVISION`]
///    instead: the words change so the notice stays true, and nobody is re-asked. `registered_with_name`
///    and `sealed_identity` (2026-09-10) are the worked example — both answer "how is your sign-in
///    stored", which is the whole of what the version-6 question put, so re-asking for them would
///    have charged a person consent for a more precise answer to the question they already
///    answered. Derivability from a declared field is a SUFFICIENT reason to be in this bucket,
///    not the boundary of it.
///
/// The incentive gradient runs towards never bumping the first kind — every bump costs consent —
/// which is why "does this stay inside the purpose the notice describes" is a written rule with a
/// test behind it (`telemetry::storage`'s `the_storage_report_fields_are_a_documented_list`)
/// rather than a judgement made in review and forgotten.
// Version 4 combines the compatibility/network dimensions introduced by version 3 with handled
// playback-error events and their bounded typed breadcrumb sequence. Existing version-3 answers
// covered the former but not the latter, so they must be asked again rather than silently expanded.
//
// The crash-report identifier (`errors_id`, 2026-09-04) is a new collected field and did NOT bump
// this, by decision: no shipped build has ever carried telemetry (v0.5.0 predates the module), so
// there is no version-4 answer in the world to expand — only the maintainer's own debug installs,
// which are re-answered by hand. The first release that ships the question ships it with the
// identifier already in it. The rule above stands for every bump after that one.
//
// Version 5 (issue #75) adds the handled sign-in error event — a new report a version-4 answer
// never covered, since sign-in was previously invisible to this channel by construction (a failed
// sign-in never reaches an authorized account, and `maybe_ask_consent` only asks once one
// exists). Every existing version-4 answer must be asked again.
//
// Version 6 (issue #76) adds three new facts: a `session_storage` class riding every usage event
// (how this install's saved sign-in is protected — never key material, ciphertext or plaintext,
// the same closed vocabulary as `rtkmem`/`install`), a handled `StorageError` report on the
// Crashes/Errors channel when a seal/open attempt fails, and the same `session_storage` class
// added to the handled sign-in error report (`signin.storage`/`contexts.signin.storage`). None of
// the three was covered by a version-5 answer.
pub(crate) const POLICY_VERSION: u32 = 6;

/// One row per version whose bump changed what is collected, for [`reask_note`] to explain a
/// re-ask to the television owner it is re-asking. **Add a row here in the SAME change that bumps
/// [`POLICY_VERSION`]** — that is the whole point of the table: a bump with no row here still
/// compiles, and [`every_reasked_version_has_a_row`] is what catches the omission.
const REASK_CHANGES: &[(u32, &str)] = &[
    (
        4,
        "Crash reports can now include a playback error report with its steps.",
    ),
    (
        5,
        "Crash reports can now include a sign-in error report.",
    ),
    (
        6,
        "Reports can now say how your sign-in is stored.",
    ),
];

// ---- Model stage M1: per-category consent SCOPE (owner decision, 2026-09-10) -------------------
//
// The rule above this line — one `POLICY_VERSION` shared by both channels, and a bump re-asks
// EVERYONE, about EVERYTHING — is too coarse: "updated the privacy notice → ask everyone again."
// [`POLICY_VERSION`]/[`REASK_CHANGES`]/[`apply`]/[`reask_note`]/[`should_ask`] above stay exactly
// as they were for the one caller that still asks the monolithic question this way — `ui::consent`'s
// first-run screen and its Settings toggle, not yet rebuilt for this model (that is its own,
// later change). Everything below is the model those call sites will move onto: **a notice
// revision (text edits) never re-asks; a consent SCOPE expansion re-asks only the people who have
// that category ON, only for that category; a stored No stays No; an event belonging to a scope a
// person has not yet accepted is held back, never the whole category.**

/// The version of *the words on the consent screen*, with no bearing on who gets asked again.
/// Bumping this changes what the notice SAYS without re-opening the question for anybody — the
/// counterpart to [`ERRORS_SCOPE`]/[`USAGE_SCOPE`], which are what actually gates a re-ask. Not
/// stored on [`Consent`]: nothing here depends on which revision a person last read, because a
/// pure text edit never needs to be told apart from the current one.
///
/// **Not yet read anywhere.** It belongs to the redesigned consent SCREEN this model change is
/// preparing (`ui::consent` still asks the old monolithic question, per-category and all — see
/// [`should_ask`]'s doc), so it has no caller yet; kept and named now so a future text-only notice
/// edit has an obvious place to record "this changed the words, not what is collected".
// Revision 2 (issue #76 review): the notice gained the cross-launch storage-probe paragraph and
// the `write_failed` stage — words changed, not what is collected, so this bumps alone with no
// re-ask. This is the exact edit the doc above asks a text-only notice change to record here.
// Revision 3 (2026-09-10): the storage report gained two facts about the SAME thing scope 6
// already covers — "how your sign-in is stored" — so the notice describes them and nothing is
// re-asked. `registered_with_name` (the app registered under a fixed bus NAME rather than as an
// application service) and `sealed_identity` (which of those identities protects the saved
// sign-in this particular report is about, or `none`). Both fit the data and the purpose the
// version-6 question already put; neither widens either, so `ERRORS_SCOPE` stays at 6 — the exact
// case the notice's own "a new field that still fits the data and purpose you already read about"
// sentence describes.
#[allow(dead_code)]
pub(crate) const NOTICE_REVISION: u32 = 3;

/// The Crash reports channel's collected-data scope. Grew at 4 (playback error report), 5
/// (sign-in error report) and 6 (storage facts — `StorageError`, and `session_storage` riding the
/// sign-in error report). A person whose accepted [`Consent::errors_scope`] is below this has a
/// pending extension for [`Category::Errors`] — see [`pending_extensions`].
pub(crate) const ERRORS_SCOPE: u32 = 6;

/// The Product analytics channel's collected-data scope. Grew at 6 only (the `session_storage`
/// property riding every usage event) — usage never grew at 4 or 5, which is why a migrated
/// version-4 or version-5 answer's [`scope_at_policy_version`] for usage is *itself*, not 0: those
/// answers already covered every usage field that existed at the time.
pub(crate) const USAGE_SCOPE: u32 = 6;

/// The two channels a consent scope can independently grow for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Category {
    Errors,
    Usage,
}

/// One row per (category, scope) bump, for [`extension_note`] and for [`scope_at_policy_version`]'s
/// migration math. **Add a row here in the SAME change that bumps [`ERRORS_SCOPE`] or
/// [`USAGE_SCOPE`]** — [`every_scope_bump_has_a_row`] catches the omission.
const SCOPE_CHANGES: &[(Category, u32, &str)] = &[
    (
        Category::Errors,
        4,
        "Crash reports can now include a playback error report with its steps.",
    ),
    (
        Category::Errors,
        5,
        "Crash reports can now include a sign-in error report.",
    ),
    (
        Category::Errors,
        6,
        "Reports can now say how your sign-in is stored.",
    ),
    (
        Category::Usage,
        6,
        "Reports can now include how your sign-in is stored.",
    ),
];

/// The scope `cat` had as of the (now-retired) monolithic `POLICY_VERSION` numbered `version` —
/// used only to migrate an old single-version file into the two-scope model. The most recent
/// [`SCOPE_CHANGES`] row for `cat` at or before `version`, or `version` itself when `cat` had no
/// row that old: usage never bumped at versions 4 or 5, so a version-4 "yes" to usage already
/// covered everything usage had at version 4 — which, under the old joint numbering, is exactly
/// what answering "yes" at version 4 meant for a field set that had not yet grown.
fn scope_at_policy_version(version: u32, cat: Category) -> u32 {
    SCOPE_CHANGES
        .iter()
        .filter(|&&(c, v, _)| c == cat && v <= version)
        .map(|&(_, v, _)| v)
        .max()
        .unwrap_or(version)
}

/// The current scope for `cat` — [`ERRORS_SCOPE`] or [`USAGE_SCOPE`].
fn current_scope(cat: Category) -> u32 {
    match cat {
        Category::Errors => ERRORS_SCOPE,
        Category::Usage => USAGE_SCOPE,
    }
}

/// Why is this television being asked again? `None` when there is nothing to explain —
/// `previous == 0` (never asked; this is a first run, not a re-ask) or `previous >=
/// POLICY_VERSION` (not a re-ask at all, the caller answered the current question already).
/// Otherwise names every collected-data change strictly after `previous` and up to
/// [`POLICY_VERSION`], accumulated in order, so a 3→5 jump names both the version-4 and the
/// version-5 change in one sentence.
pub(crate) fn reask_note(previous: u32) -> Option<&'static str> {
    if previous == 0 || previous >= POLICY_VERSION {
        return None;
    }
    let mut note = String::from("Asking again because what is collected has changed: ");
    let mut wrote_any = false;
    for &(version, what_changed) in REASK_CHANGES {
        if version > previous && version <= POLICY_VERSION {
            if wrote_any {
                note.push(' ');
            }
            note.push_str(what_changed);
            wrote_any = true;
        }
    }
    wrote_any.then(|| &*Box::leak(note.into_boxed_str()))
}

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
    /// never to PostHog. It is what makes "users affected" a count of Crash report IDs — one per
    /// uninterrupted opt-in on a television — instead of a count of events. Independent of [`Self::install_id`] in
    /// both directions: minted, kept and destroyed by its own switch alone — and by sign-out,
    /// which destroys both.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub errors_id: Option<String>,
    /// The [`ERRORS_SCOPE`] this person actually ACCEPTED for the Crash reports channel — `0` when
    /// [`Self::errors`] is off (irrelevant) or the question has never been engaged at all. Never
    /// rewritten retroactively: a scope bump changes [`ERRORS_SCOPE`], not this field, until the
    /// person answers the extension it creates.
    #[serde(default)]
    pub errors_scope: u32,
    /// The [`USAGE_SCOPE`] equivalent of [`Self::errors_scope`], for Product analytics.
    #[serde(default)]
    pub usage_scope: u32,
    /// **The Crash reports channel's own "no, not that expansion" memory.** The [`ERRORS_SCOPE`]
    /// this person was looking at the LAST TIME they declined an extension for this category — `0`
    /// means never declined one. Owner decision, 2026-09-10: **a No to an extension is not a
    /// withdrawal.** It leaves [`Self::errors`]/[`Self::errors_id`]/[`Self::errors_scope`] exactly
    /// where they were — the category keeps sending at the scope already accepted — and this field
    /// alone is what stops [`pending_extensions`] from asking the same question again the very next
    /// boot. It is cleared the moment the scope grows past it (a genuinely NEW expansion, which
    /// deserves its own question) or the category is explicitly turned on through Settings (a fresh
    /// opt-in has nothing pending to remember a No against). Withdrawing the category through
    /// Settings' OFF switch is the one door that actually turns [`Self::errors`] off, and it clears
    /// this too, since a category that is off has nothing extension-pending to decline.
    #[serde(default)]
    pub errors_declined_scope: u32,
    /// [`Self::errors_declined_scope`]'s twin for Product analytics.
    #[serde(default)]
    pub usage_declined_scope: u32,
}

impl Consent {
    /// Has this person been asked, against the CURRENT policy? A bump re-asks.
    ///
    /// This is the LEGACY monolithic gate: `asked_version` is set once, by [`apply`]'s full
    /// question, to [`POLICY_VERSION`] — which this maintenance line freezes at 6 — and never again,
    /// because a collected-data change after that point grows [`ERRORS_SCOPE`]/[`USAGE_SCOPE`]
    /// instead. So `answered()` stays true forever once the first-run screen has been answered, and
    /// it is deliberately NOT how [`allows_errors`]/[`allows_usage`] are gated — see their docs.
    pub(crate) fn answered(&self) -> bool {
        self.asked_version >= POLICY_VERSION
    }
    /// Has this person engaged the consent question AT ALL, at any scope? Distinct from
    /// [`Self::answered`], which asks about the current (frozen) policy version specifically —
    /// `ever_answered` is what tells a genuinely fresh install apart from one whose decision merely
    /// predates a later scope bump, which `diag::event`'s sign-in defer needs: a person who already
    /// decided (even a stale "yes") is not the case that funnel exists for.
    pub(crate) fn ever_answered(&self) -> bool {
        self.asked_version > 0
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
///
/// **Model stage M1 (owner decision, 2026-09-10):** a fresh install (`asked_version == 0`) gets the
/// full first-run question, exactly as before. Otherwise the question is never "has the policy
/// moved" — it is [`pending_extensions`]: are there categories this person has ON whose accepted
/// scope trails [`ERRORS_SCOPE`]/[`USAGE_SCOPE`]? A category left OFF is never re-asked by a scope
/// bump, because a stored No is a real decision, not an unanswered one — and neither is a category
/// already accepted at the current scope.
pub(crate) fn should_ask(c: &Consent, automated: bool) -> bool {
    if automated {
        return false;
    }
    if c.asked_version == 0 {
        return !c.answered();
    }
    !pending_extensions(c).is_empty()
}

/// Which categories does `c` have ON whose accepted scope trails the current one? Empty for a
/// category that is OFF (a stored No is never re-asked), for one already at
/// [`ERRORS_SCOPE`]/[`USAGE_SCOPE`], and — since the 2026-09-10 scope-versions model — for one
/// whose most recent DECLINE already covers the current scope
/// (`declined_scope >= current_scope`): a No is a real decision and is not re-asked until the
/// scope grows PAST what was declined. A later expansion (current scope > declined scope) makes
/// the category pending again — the gap widened after the No — and [`extension_note`] then lists
/// every row above the ACCEPTED scope, not just the ones past the decline, so a second question
/// re-offers the whole gap rather than only the newest sliver.
pub(crate) fn pending_extensions(c: &Consent) -> Vec<Category> {
    let mut v = Vec::new();
    if c.errors && c.errors_scope < ERRORS_SCOPE && c.errors_declined_scope < ERRORS_SCOPE {
        v.push(Category::Errors);
    }
    if c.usage && c.usage_scope < USAGE_SCOPE && c.usage_declined_scope < USAGE_SCOPE {
        v.push(Category::Usage);
    }
    v
}

/// Why is `cat` being asked again, for the television owner this is re-asking? `None` when `cat`
/// is not actually pending (nothing to explain) — otherwise every [`SCOPE_CHANGES`] row for `cat`
/// strictly after its ACCEPTED scope and up to the current one, accumulated in order, so a 4→6
/// jump names both the version-5 and version-6 change for that category alone.
///
/// **Deliberately keyed off the accepted scope, never the declined one.** A person who said No at
/// scope 4 and is now pending again because the scope grew to 6 sees the SAME note a person who
/// had never been asked before would see for that same 4→6 gap — every row since what they last
/// actually agreed to, including the one they already declined. The alternative (naming only the
/// rows past the decline) would silently under-explain a second question: the person never accepted
/// the version-5 change either, so leaving it out of the note reads as though it were settled.
///
/// **Model stage M2's caller**: `ui::consent::open` calls this once per category at ceremony
/// open (never from the draw path — it builds a fresh owned `String` on every call) and leaks the
/// result for the extension screen's note line.
pub(crate) fn extension_note(c: &Consent, cat: Category) -> Option<String> {
    if !pending_extensions(c).contains(&cat) {
        return None;
    }
    let accepted = match cat {
        Category::Errors => c.errors_scope,
        Category::Usage => c.usage_scope,
    };
    let current = current_scope(cat);
    let mut note = String::from("Asking again because what is collected has changed: ");
    let mut wrote_any = false;
    for &(row_cat, version, what_changed) in SCOPE_CHANGES {
        if row_cat == cat && version > accepted && version <= current {
            if wrote_any {
                note.push(' ');
            }
            note.push_str(what_changed);
            wrote_any = true;
        }
    }
    wrote_any.then_some(note)
}

/// Migrate a file written before per-category scope existed. A file with `asked_version >= 1` (it
/// was answered against the old monolithic policy) and a category flag ON gets that category's
/// `_scope` field backfilled to [`scope_at_policy_version`] — **only when the loaded scope is still
/// `0`**, so this is a no-op on a file the current code already wrote (idempotent to call on every
/// load) and never overwrites an accepted scope retroactively. A flag OFF needs no scope at all.
pub(crate) fn migrate_loaded(mut c: Consent) -> Consent {
    if c.asked_version >= 1 {
        if c.errors && c.errors_scope == 0 {
            c.errors_scope = scope_at_policy_version(c.asked_version, Category::Errors);
        }
        if c.usage && c.usage_scope == 0 {
            c.usage_scope = scope_at_policy_version(c.asked_version, Category::Usage);
        }
    }
    c
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
/// Six behaviours, and each is a test below:
/// * enabling a channel, with no identifier yet, mints one for THAT channel;
/// * enabling it again does NOT re-mint;
/// * disabling a channel DROPS its identifier, independently of the other channel;
/// * a channel whose mint returns `None` is recorded as off, and the other channel still counts;
/// * the answer is recorded against the current [`POLICY_VERSION`] either way, so a "no" is a real
///   answer and is not re-asked until the policy itself changes;
/// * **a channel that was ALREADY on and stays on keeps its OLD accepted scope**, rather than
///   silently being bumped to the current one — this is also the door Settings edits go through
///   (`ui::consent::record_answer`'s non-extension branch), and a person editing one category must
///   not have the OTHER category's still-pending scope extension accepted for them as a side
///   effect of that edit. Only a channel that is genuinely NEWLY turned on here (off in `prev`,
///   on now) gets the current scope — that press really is an answer to the current question for
///   that channel, the same as a fresh first-run opt-in. A category with a pending extension that
///   stays on unchanged keeps `pending_extensions`/`should_ask` naming it, exactly as before this
///   edit.
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
    let errors = errors && errors_id.is_some();
    let usage = usage && install_id.is_some();
    // A category that was already on and stays on keeps its OLD accepted scope — only a channel
    // genuinely newly turned on here gets bumped to the current scope. See the doc above: this is
    // what stops a Settings edit of one category from silently accepting the other's pending scope
    // extension as a side effect.
    let errors_scope = if !errors {
        0
    } else if prev.errors {
        prev.errors_scope
    } else {
        current_scope(Category::Errors)
    };
    let usage_scope = if !usage {
        0
    } else if prev.usage {
        prev.usage_scope
    } else {
        current_scope(Category::Usage)
    };
    // A category turned OFF here has nothing extension-pending to remember a No against; a
    // category genuinely turned ON here (was off in `prev`) is a fresh opt-in, and a fresh opt-in
    // has no prior decline either — see `Consent::errors_declined_scope`'s doc. Only a category
    // that was ALREADY on and stays on keeps whatever it had declined, exactly like its scope above.
    let errors_declined_scope = if !errors || !prev.errors {
        0
    } else {
        prev.errors_declined_scope
    };
    let usage_declined_scope = if !usage || !prev.usage {
        0
    } else {
        prev.usage_declined_scope
    };
    Consent {
        asked_version: POLICY_VERSION,
        errors,
        usage,
        install_id,
        errors_id,
        errors_scope,
        usage_scope,
        errors_declined_scope,
        usage_declined_scope,
    }
}

/// **The scope-EXTENSION transition.** Apply a person's answer to exactly ONE pending category —
/// see [`pending_extensions`] — leaving the other category's flag, identifier and accepted scope
/// entirely untouched either way. `mint` behaves exactly as in [`apply`]: called at most once, only
/// when this category actually needs a fresh identifier (in practice never here, since
/// [`pending_extensions`] only names a category that is already on and so already holds one), and a
/// `None` from it refuses the extension the same way a failed mint refuses a fresh opt-in.
///
/// **Owner decision, 2026-09-10: a No to an extension is not a withdrawal.** A "Yes" KEEPS the
/// existing identifier (an extension is not a fresh opt-in) and raises the ACCEPTED
/// `errors_scope`/`usage_scope` to the current one, clearing any earlier decline. A "No" changes
/// NOTHING about whether the category is on, its identifier, or its accepted scope — the category
/// keeps sending at exactly the scope it already agreed to, nothing of the NEW scope goes out
/// ([`pending_extensions`]/`allows_*_at` are what withhold that), and the only field this writes is
/// `errors_declined_scope`/`usage_declined_scope`, so the same question is not asked again until
/// the scope grows further still. Withdrawing the category is a different door entirely — Settings'
/// OFF switch, through [`apply`] — and stays exactly where it always was.
///
/// **Model stage M2's caller**: `ui::consent::record_answer` folds this once per
/// [`pending_extensions`] category the open ceremony actually asked about, leaving every category
/// it did not ask about untouched — including the OTHER one, on a ceremony asking about only one.
///
/// **Also stamps [`Consent::asked_version`] to [`POLICY_VERSION`].** An extension answer IS an
/// answer against the current policy — this is the question `should_ask`/`pending_extensions`
/// route to instead of the first-run one, not a lesser one — and `Consent::answered()` stays the
/// wire gate `sender::allowed`/`native::sync`/`mod.rs`'s boot line all read. Without this, an old
/// install (shipped `asked_version` 4 or 5) that accepts an extension never becomes `answered()`,
/// so every record of BOTH categories is retired unsent forever and the native crash backend never
/// arms — silently, since `allows_errors`/`allows_usage` (M1) do not depend on `answered()` and so
/// see nothing wrong. This does not skip a still-pending OTHER category's own question:
/// `pending_extensions`/`should_ask` key off the per-category `_scope` fields, which this only
/// advances for `cat`.
pub(crate) fn apply_extension(
    prev: &Consent,
    cat: Category,
    answer: bool,
    mut mint: impl FnMut() -> Option<String>,
) -> Consent {
    let mut next = prev.clone();
    next.asked_version = POLICY_VERSION;
    if !answer {
        // **Owner decision, 2026-09-10: a No to an extension is not a withdrawal.** The category
        // stays ON, at the scope it was already accepted at — its flag, identifier and accepted
        // `_scope` are untouched — and nothing of the NEW scope is sent, which is what
        // `pending_extensions`/`allows_*_at` already enforce off the unchanged accepted scope.
        // The only thing this records is the refusal itself, so the same question is not asked
        // again on the very next boot: `declined_scope` is set to the scope being declined.
        // Withdrawing the category outright stays exactly where it always was — Settings' own OFF
        // switch, through `apply`, which is the only door that actually turns a category off.
        match cat {
            Category::Errors => next.errors_declined_scope = current_scope(cat),
            Category::Usage => next.usage_declined_scope = current_scope(cat),
        }
        return next;
    }
    let mut resolve = |had_id: &Option<String>| -> Option<String> {
        match had_id {
            Some(id) if !id.is_empty() => Some(id.clone()),
            _ => mint().filter(|id| !id.is_empty()),
        }
    };
    match cat {
        Category::Errors => {
            let id = resolve(&prev.errors_id);
            next.errors = id.is_some();
            next.errors_scope = if next.errors { current_scope(cat) } else { 0 };
            next.errors_declined_scope = 0;
            next.errors_id = id;
        }
        Category::Usage => {
            let id = resolve(&prev.install_id);
            next.usage = id.is_some();
            next.usage_scope = if next.usage { current_scope(cat) } else { 0 };
            next.usage_declined_scope = 0;
            next.install_id = id;
        }
    }
    next
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
///
/// **Means "the category is ON, at ANY accepted scope"** — deliberately NOT gated on
/// [`Consent::answered`]/scope staleness, unlike before model stage M1. A person whose usage
/// consent predates the current [`USAGE_SCOPE`] still gets ordinary usage events; only the FIELDS a
/// scope bump actually added are held back, per event, by [`allows_usage_at`] — the
/// `session_storage` property is the one that exists today.
pub(crate) fn allows_usage() -> bool {
    CURRENT
        .read()
        .map(|g| g.as_ref().is_some_and(|c| c.usage))
        .unwrap_or(false)
}

/// May an ERROR report be sent? The crash channel's twin of [`allows_usage`], failing closed for
/// the same reason and meaning the same "on at any scope" thing. It gates both
/// `crashreport::report_pending` (the only thing that opens the crash log at all) and the sparse
/// in-memory playback-error trace. Consent gates collection, not just the send: a television whose
/// owner said no is neither scanned for faults nor traced.
pub(crate) fn allows_errors() -> bool {
    CURRENT
        .read()
        .map(|g| g.as_ref().is_some_and(|c| c.errors))
        .unwrap_or(false)
}

/// May a report belonging to the Crash reports channel's `scope`-numbered field set be sent? The
/// per-FIELD twin of [`allows_errors`] — the sign-in error report (scope 5), the storage error
/// report (scope 6) and the playback error report (scope 4) each ask this with their own number
/// rather than the coarse [`allows_errors`], so a person whose accepted scope trails the field's
/// own is held back on exactly that field, not the whole channel.
pub(crate) fn allows_errors_at(scope: u32) -> bool {
    CURRENT
        .read()
        .map(|g| g.as_ref().is_some_and(|c| c.errors && c.errors_scope >= scope))
        .unwrap_or(false)
}

/// The Product analytics twin of [`allows_errors_at`] — the `session_storage` property (usage
/// scope 6) is today's one caller.
pub(crate) fn allows_usage_at(scope: u32) -> bool {
    CURRENT
        .read()
        .map(|g| g.as_ref().is_some_and(|c| c.usage && c.usage_scope >= scope))
        .unwrap_or(false)
}

/// The crash-report identifier every Sentry-bound report carries, or `None` when the channel is
/// off — read from the snapshot, like the two gates, so a producer on the render thread never
/// touches the disk for it. `None` while [`allows_errors`] is true cannot happen through [`apply`],
/// but a producer must READ it rather than assume it: the failure would be a report carrying a
/// fabricated or empty id, which is the one outcome this field exists to make impossible.
pub(crate) fn errors_id() -> Option<String> {
    CURRENT
        .read()
        .ok()
        .and_then(|g| g.as_ref().filter(|c| c.errors).and_then(|c| c.errors_id.clone()))
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

    /// **Regression: a Settings edit of ONE category must not silently accept the OTHER category's
    /// pending scope extension.** `prev` is at scope 4 for both, current is 6 for both (a real
    /// shape once `ERRORS_SCOPE`/`USAGE_SCOPE` moved past this maintenance line's frozen values in
    /// a test double below) — turning usage off through `apply` (Settings' non-extension door) must
    /// leave `errors_scope` exactly where it was, since errors was never touched by this edit, and
    /// `pending_extensions` must still name it.
    #[test]
    fn a_settings_edit_of_one_category_does_not_silently_accept_the_others_pending_scope() {
        let prev = Consent {
            asked_version: 4,
            errors: true,
            errors_scope: 4,
            errors_id: Some("e".repeat(32)),
            usage: true,
            usage_scope: 4,
            install_id: Some("u".repeat(32)),
            ..Default::default()
        };
        assert_eq!(
            pending_extensions(&prev),
            vec![Category::Errors, Category::Usage]
        );

        // Turn usage OFF; errors stays on, untouched by this edit.
        let next = apply(&prev, true, false, || panic!("must not mint turning usage off"));
        assert!(next.errors && !next.usage);
        assert_eq!(
            next.errors_scope, 4,
            "errors was not touched by this edit — its scope must not silently advance to current"
        );
        assert_eq!(
            pending_extensions(&next),
            vec![Category::Errors],
            "errors' extension question must still be pending after an unrelated edit"
        );

        // A category genuinely turned ON by this same edit still gets the current scope — that
        // press really is a fresh answer for it.
        let fresh_on = apply(&next, true, true, || Some("u2".repeat(16)));
        assert_eq!(fresh_on.usage_scope, USAGE_SCOPE, "a fresh opt-in gets the current scope");
        assert_eq!(fresh_on.errors_scope, 4, "errors, still untouched, still keeps its old scope");
    }

    /// **A Settings edit's two doors onto `errors_declined_scope`.** Turning a category ON after a
    /// No is a FRESH opt-in — it gets the current scope and the decline is forgotten, exactly like
    /// a first answer, because there is nothing left pending to remember a refusal against. Turning
    /// it OFF clears the memory too, since an off category has nothing extension-pending to have
    /// declined. Neither is `apply_extension`'s door (that one records a decline without touching
    /// the flag) — this is Settings' own on/off switch, through `apply`.
    #[test]
    fn a_settings_edit_turning_a_category_on_after_a_no_clears_the_decline_and_off_clears_both() {
        let declined = Consent {
            asked_version: POLICY_VERSION,
            errors: true,
            errors_scope: 4,
            errors_declined_scope: ERRORS_SCOPE,
            errors_id: Some("e".repeat(32)),
            ..Default::default()
        };
        assert!(pending_extensions(&declined).is_empty(), "the No still stands");

        // OFF then back ON through Settings — a fresh opt-in, not a re-acceptance of the old
        // decline.
        let off = apply(&declined, false, false, || panic!("no mint on a withdrawal"));
        assert_eq!(off.errors_scope, 0);
        assert_eq!(off.errors_declined_scope, 0, "an off category remembers no decline");

        let back_on = apply(&off, true, false, || Some("fresh".into()));
        assert_eq!(
            back_on.errors_scope, ERRORS_SCOPE,
            "a fresh opt-in gets the current scope, like any first answer"
        );
        assert_eq!(
            back_on.errors_declined_scope, 0,
            "the earlier No is forgotten — it belonged to the identity that was withdrawn"
        );
        assert!(pending_extensions(&back_on).is_empty());

        // Staying ON across an unrelated edit keeps the memory of the decline exactly as it was.
        let unrelated_edit = apply(&declined, true, true, || Some("u".repeat(16)));
        assert_eq!(
            unrelated_edit.errors_declined_scope, ERRORS_SCOPE,
            "errors stayed on through this edit, so its decline is untouched"
        );
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

    /// The crash-report id accessor reads the SNAPSHOT and fails closed on the flag: an unanswered
    /// decision or the wrong channel yields no id, but — model stage M1 — a stale ACCEPTED SCOPE no
    /// longer withholds it, because the coarse channel and its baseline crash reports stay valid
    /// across a scope bump; only the NEW per-field report types are scope-gated, and that gate is
    /// [`allows_errors_at`], tested separately.
    #[test]
    fn the_errors_id_accessor_fails_closed_on_the_flag_not_on_scope_staleness() {
        let _g = crate::testlock::serial();
        let saved = CURRENT.read().ok().and_then(|g| g.clone());
        install(Consent {
            asked_version: 4,
            errors: true,
            errors_scope: 4,
            errors_id: Some("stale-scope".into()),
            ..Default::default()
        });
        assert!(
            allows_errors(),
            "a stale ACCEPTED SCOPE still authorises the baseline crash-report channel"
        );
        assert_eq!(errors_id().as_deref(), Some("stale-scope"));
        install(Consent::default());
        assert!(!allows_errors(), "an unanswered decision fails closed");
        assert!(errors_id().is_none());
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

    // ---- Model stage M1: per-category consent scope versions --------------------------------

    /// (a) **A stored No for both categories is never re-asked by any scope bump.** Neither
    /// category is ON, so `pending_extensions` is empty regardless of how stale `asked_version` or
    /// the (irrelevant, `0`) scope fields are — a refusal is a real decision, not an unanswered one.
    #[test]
    fn a_stored_no_for_both_categories_is_never_reasked_by_a_scope_bump() {
        let both_refused = Consent {
            asked_version: 4,
            errors: false,
            usage: false,
            ..Default::default()
        };
        assert!(pending_extensions(&both_refused).is_empty());
        assert!(!should_ask(&both_refused, false));
    }

    /// (b) **errors=on at scope 4, usage=off, current 6 → asked only about errors**, and answering
    /// Yes sets `errors_scope` to the current scope while No turns errors off and destroys its
    /// identifier — usage, being off, is neither pending nor touched by either answer.
    #[test]
    fn an_extension_asks_only_the_enabled_category_and_apply_extension_resolves_it() {
        let c = Consent {
            asked_version: 4,
            errors: true,
            errors_scope: 4,
            errors_id: Some("e".repeat(32)),
            usage: false,
            usage_scope: 0,
            install_id: None,
            ..Default::default()
        };
        assert_eq!(pending_extensions(&c), vec![Category::Errors]);
        assert!(should_ask(&c, false));

        let yes = apply_extension(&c, Category::Errors, true, || Some("e2".repeat(16)));
        assert!(yes.errors);
        assert_eq!(yes.errors_scope, ERRORS_SCOPE);
        assert_eq!(
            yes.errors_id.as_deref(),
            Some("e".repeat(32).as_str()),
            "an extension KEEPS the existing identifier — it is not a fresh opt-in"
        );
        assert_eq!(yes.errors_declined_scope, 0, "a Yes clears any earlier decline");
        assert!(!yes.usage && yes.usage_scope == 0 && yes.install_id.is_none());
        assert!(pending_extensions(&yes).is_empty());

        // Owner decision, 2026-09-10: a No to an extension is not a withdrawal — it leaves the
        // category exactly as it was (still on, same scope, same identifier) and records only the
        // refusal, so nothing mints and nothing is dropped.
        let no = apply_extension(&c, Category::Errors, false, || {
            panic!("must not mint on a refusal")
        });
        assert!(
            no.errors && no.errors_scope == 4 && no.errors_id.as_deref() == Some("e".repeat(32).as_str()),
            "a No keeps the category on, at its already-accepted scope, with its identifier intact"
        );
        assert_eq!(
            no.errors_declined_scope, ERRORS_SCOPE,
            "the refusal is recorded against the scope that was actually declined"
        );
        assert!(
            pending_extensions(&no).is_empty(),
            "a declined scope is not re-asked until the scope grows past it"
        );
        assert!(!no.usage && no.usage_scope == 0 && no.install_id.is_none());
    }

    /// A decline that still covers the current scope is not re-asked; one a later expansion has
    /// outgrown is — and the note it is re-asked with names every row since the ACCEPTED scope,
    /// not just the ones past the decline, so a second question re-offers the whole gap
    /// (`Consent::errors_declined_scope`'s doc makes the same promise).
    #[test]
    fn a_further_expansion_past_a_decline_asks_again_and_the_note_covers_the_whole_gap() {
        // Declined exactly at the current scope: nothing has grown since, so the No still stands.
        let declined_at_current = Consent {
            asked_version: POLICY_VERSION,
            errors: true,
            errors_scope: 2,
            errors_declined_scope: ERRORS_SCOPE,
            errors_id: Some("e".repeat(32)),
            ..Default::default()
        };
        assert!(
            pending_extensions(&declined_at_current).is_empty(),
            "the decline still covers the current scope — nothing has grown past it"
        );
        assert!(extension_note(&declined_at_current, Category::Errors).is_none());

        // The scope has since grown past what was declined (a real shape once ERRORS_SCOPE moves
        // past 4 again in the future; today it exercises the same math with 4 standing in for "an
        // earlier decline"). Pending again, and the note lists every row after the ACCEPTED scope
        // (2) up to the current one — including the version-4 row the person already said no to —
        // not merely the rows strictly after the decline.
        let declined_then_outgrown = Consent {
            errors_declined_scope: 4,
            ..declined_at_current.clone()
        };
        assert_eq!(
            pending_extensions(&declined_then_outgrown),
            vec![Category::Errors]
        );
        let note = extension_note(&declined_then_outgrown, Category::Errors)
            .expect("pending again — there is something to explain");
        for &(cat, version, what_changed) in SCOPE_CHANGES {
            if cat == Category::Errors && version > 2 && version <= ERRORS_SCOPE {
                assert!(
                    note.contains(what_changed),
                    "the note dropped a row the person never actually accepted: {what_changed:?}"
                );
            }
        }
    }

    /// **Regression: accepting a pending extension must make `Consent::answered()` true again**,
    /// because `sender::allowed`/`native::sync` still gate on it. Built from a REALISTIC migrated
    /// shape — `asked_version` 4 (a v0.6.0-shipped file), scope backfilled by `migrate_loaded` —
    /// rather than the M2 test helper's `asked_version: POLICY_VERSION` shape, which no loaded file
    /// can actually have while a scope trails it.
    #[test]
    fn accepting_an_extension_from_a_migrated_file_makes_answered_true_and_unblocks_sending() {
        let old = Consent {
            asked_version: 4,
            errors: true,
            errors_id: Some("e".repeat(32)),
            errors_scope: 0,
            ..Default::default()
        };
        let migrated = migrate_loaded(old);
        assert_eq!(migrated.errors_scope, 4);
        assert!(!migrated.answered(), "a version-4 file is stale against POLICY_VERSION 6");
        assert_eq!(pending_extensions(&migrated), vec![Category::Errors]);

        let next = apply_extension(&migrated, Category::Errors, true, || Some("e2".repeat(16)));
        assert!(
            next.answered(),
            "accepting the extension is an answer against the current policy"
        );

        let record = super::super::queue::Record {
            category: super::super::queue::Category::Errors,
            dest: super::super::queue::Dest::Sentry,
            event_id: "id".into(),
            body: Vec::new(),
        };
        assert!(
            super::super::sender::allowed(&record, &next),
            "an accepted extension must unblock sending, not leave every record retired unsent"
        );

        let declined = apply_extension(&migrated, Category::Errors, false, || {
            panic!("must not mint on a refusal")
        });
        assert!(declined.answered(), "a No to the extension is also a real answer");
        assert!(
            super::super::sender::allowed(&record, &declined),
            "a No to an extension is not a withdrawal — the category stays ON at its already-\
             accepted scope, so its ordinary (non-scope-gated) records still flow"
        );
        assert!(
            declined.errors && declined.errors_scope < ERRORS_SCOPE,
            "but the accepted scope did not move, so the NEW scope's own fields stay withheld"
        );
    }

    /// (c) **errors on at scope 4 while current is 6: a crash report is still sent, a
    /// `StorageError` is NOT, and after Yes it is.** The coarse `allows_errors` gate is
    /// scope-blind; `allows_errors_at` is what a NEW field type (storage scope 6) asks instead.
    #[test]
    fn a_pending_extension_still_sends_baseline_reports_but_withholds_the_new_field() {
        let _g = crate::testlock::serial();
        let saved = CURRENT.read().ok().and_then(|g| g.clone());
        let stale = Consent {
            asked_version: 4,
            errors: true,
            errors_scope: 4,
            errors_id: Some("e".repeat(32)),
            ..Default::default()
        };
        install(stale.clone());
        assert!(allows_errors(), "an ordinary crash report is still sent");
        assert!(
            !allows_errors_at(6),
            "a StorageError (errors scope 6) is withheld until the extension is accepted"
        );
        let yes = apply_extension(&stale, Category::Errors, true, || Some("e2".repeat(16)));
        install(yes);
        assert!(allows_errors_at(6), "accepting the extension unlocks it");
        if let Ok(mut g) = CURRENT.write() {
            *g = saved;
        }
    }

    /// **Requirement 4, Errors: a DECLINED scope keeps the new field withheld while ordinary crash
    /// reports still flow.** Twin of the accept-side test above, over the refusal path — declining
    /// must not silently unlock the very field the person said no to.
    #[test]
    fn a_declined_errors_extension_still_sends_baseline_reports_but_withholds_the_new_field() {
        let _g = crate::testlock::serial();
        let saved = CURRENT.read().ok().and_then(|g| g.clone());
        let stale = Consent {
            asked_version: 4,
            errors: true,
            errors_scope: 4,
            errors_id: Some("e".repeat(32)),
            ..Default::default()
        };
        let no = apply_extension(&stale, Category::Errors, false, || {
            panic!("must not mint on a refusal")
        });
        install(no);
        assert!(allows_errors(), "an ordinary crash report is still sent after a No");
        assert!(
            !allows_errors_at(6),
            "the StorageError field stays withheld — that is exactly what was declined"
        );
        if let Ok(mut g) = CURRENT.write() {
            *g = saved;
        }
    }

    /// **Requirement 4, Usage: the Product analytics twin.** A declined `session_storage` extension
    /// leaves ordinary usage events flowing while withholding the one field the scope bump added.
    #[test]
    fn a_declined_usage_extension_still_sends_baseline_events_but_withholds_the_new_field() {
        let _g = crate::testlock::serial();
        let saved = CURRENT.read().ok().and_then(|g| g.clone());
        let stale = Consent {
            asked_version: 4,
            usage: true,
            usage_scope: 4,
            install_id: Some("u".repeat(32)),
            ..Default::default()
        };
        assert!(pending_extensions(&stale).contains(&Category::Usage));
        let no = apply_extension(&stale, Category::Usage, false, || {
            panic!("must not mint on a refusal")
        });
        install(no.clone());
        assert!(allows_usage(), "an ordinary usage event is still sent after a No");
        assert!(
            !allows_usage_at(6),
            "the session_storage property stays withheld — that is exactly what was declined"
        );
        assert!(pending_extensions(&no).is_empty(), "the No is not re-asked immediately");

        let yes = apply_extension(&stale, Category::Usage, true, || Some("u2".repeat(16)));
        install(yes);
        assert!(allows_usage_at(6), "accepting it afterwards unlocks the field");
        if let Ok(mut g) = CURRENT.write() {
            *g = saved;
        }
    }

    /// (d) **Migration**: an old file (`asked_version` 4, both flags true, no scope fields) yields
    /// `errors_scope == 4` and `usage_scope == 4` — usage never bumped before 6, so its scope at an
    /// old asked_version is the asked_version itself, not `0` — and both categories are pending
    /// against the current scope of 6.
    #[test]
    fn migrating_an_old_file_backfills_both_scopes_from_its_asked_version() {
        let old = Consent {
            asked_version: 4,
            errors: true,
            usage: true,
            errors_id: Some("e".repeat(32)),
            install_id: Some("u".repeat(32)),
            errors_scope: 0,
            usage_scope: 0,
            ..Default::default()
        };
        let migrated = migrate_loaded(old);
        assert_eq!(migrated.errors_scope, 4);
        assert_eq!(
            migrated.usage_scope, 4,
            "usage never bumped before 6, so a version-4 answer already covered it whole"
        );
        assert_eq!(
            pending_extensions(&migrated),
            vec![Category::Errors, Category::Usage]
        );

        // A category left OFF gets no scope at all, migrated or not.
        let errors_only = Consent {
            asked_version: 5,
            errors: true,
            usage: false,
            errors_id: Some("e".repeat(32)),
            ..Default::default()
        };
        let migrated = migrate_loaded(errors_only);
        assert_eq!(migrated.errors_scope, 5, "the most recent errors row at or before 5");
        assert_eq!(migrated.usage_scope, 0, "usage is off — nothing to migrate");

        // Idempotent: migrating an already-current file changes nothing.
        let current = apply(&Consent::default(), true, true, || Some("x".repeat(32)));
        assert_eq!(migrate_loaded(current.clone()), current);
    }

    /// (e) **A `NOTICE_REVISION` bump alone re-asks nobody** — it has no bearing on `should_ask` at
    /// all, which is the whole point of separating it from the per-category scopes.
    #[test]
    fn a_notice_revision_bump_alone_reasks_nobody() {
        let fully_current = apply(&Consent::default(), true, true, || Some("x".repeat(32)));
        assert!(!should_ask(&fully_current, false));
        // NOTICE_REVISION does not even appear in `Consent` or in `should_ask`'s inputs — a bump to
        // it cannot change this answer, by construction rather than by one sampled value. Naming it
        // here keeps the constant from silently losing its one documented property unnoticed.
        let _ = NOTICE_REVISION;
    }

    /// (f) **A fresh install is asked the full question**, unaffected by any of the above.
    #[test]
    fn a_fresh_install_is_asked_the_full_question() {
        assert!(should_ask(&Consent::default(), false));
    }

    /// `scope_at_policy_version` is the migration primitive (d) relies on: the most recent
    /// [`SCOPE_CHANGES`] row for a category at or before an old version, or the version itself when
    /// the category had no row that old.
    #[test]
    fn scope_at_policy_version_finds_the_most_recent_row_or_falls_back_to_the_version() {
        assert_eq!(scope_at_policy_version(4, Category::Errors), 4);
        assert_eq!(scope_at_policy_version(5, Category::Errors), 5);
        assert_eq!(scope_at_policy_version(6, Category::Errors), 6);
        assert_eq!(
            scope_at_policy_version(3, Category::Errors),
            3,
            "no Errors row is this old — the version itself is the fallback"
        );
        assert_eq!(
            scope_at_policy_version(4, Category::Usage),
            4,
            "Usage's only row is 6, so a version-4 answer falls back to itself"
        );
        assert_eq!(scope_at_policy_version(6, Category::Usage), 6);
    }

    /// Every [`ERRORS_SCOPE`]/[`USAGE_SCOPE`] bump has a [`SCOPE_CHANGES`] row for that category —
    /// the per-category twin of [`every_reasked_version_has_a_row`], catching a scope bump with no
    /// explanatory row the same way.
    #[test]
    fn every_scope_bump_has_a_row() {
        for v in 4..=ERRORS_SCOPE {
            assert!(
                SCOPE_CHANGES
                    .iter()
                    .any(|&(c, version, _)| c == Category::Errors && version == v),
                "errors scope {v} has no SCOPE_CHANGES row"
            );
        }
        assert!(
            SCOPE_CHANGES
                .iter()
                .any(|&(c, version, _)| c == Category::Usage && version == USAGE_SCOPE),
            "usage scope {USAGE_SCOPE} has no SCOPE_CHANGES row"
        );
    }

    /// [`extension_note`] names only the rows strictly between the accepted and current scope, and
    /// is `None` once nothing is pending.
    #[test]
    fn extension_note_names_only_the_rows_still_pending() {
        let c = Consent {
            asked_version: 4,
            errors: true,
            errors_scope: 4,
            errors_id: Some("e".repeat(32)),
            ..Default::default()
        };
        let note = extension_note(&c, Category::Errors).expect("errors is pending");
        assert!(note.contains("sign-in error report"));
        assert!(note.contains("how your sign-in is stored"));
        assert!(!note.contains("playback error report"), "4 was already accepted");

        assert_eq!(
            extension_note(&c, Category::Usage),
            None,
            "usage is off — nothing pending, nothing to explain"
        );

        let current = apply(&Consent::default(), true, true, || Some("x".repeat(32)));
        assert_eq!(extension_note(&current, Category::Errors), None);
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

    /// Never asked (`0`) is a first run, not a re-ask, and has nothing to explain.
    #[test]
    fn reask_note_is_none_when_never_asked() {
        assert_eq!(reask_note(0), None);
    }

    /// Already answered against the current policy: nothing to explain either.
    #[test]
    fn reask_note_is_none_when_already_current() {
        assert_eq!(reask_note(POLICY_VERSION), None);
    }

    /// A 4→6 re-ask accumulates the version-5 and version-6 changes, in order.
    #[test]
    fn reask_note_from_four_names_signin_and_storage_changes() {
        assert_eq!(
            reask_note(4),
            Some(
                "Asking again because what is collected has changed: Crash reports can now \
                 include a sign-in error report. Reports can now say how your sign-in is \
                 stored."
            )
        );
    }

    /// A 5→6 re-ask names only the version-6 storage change.
    #[test]
    fn reask_note_from_five_names_only_the_storage_change() {
        assert_eq!(
            reask_note(5),
            Some(
                "Asking again because what is collected has changed: Reports can now say how \
                 your sign-in is stored."
            )
        );
    }

    /// A 3→6 re-ask accumulates the version-4, version-5 and version-6 changes, in order.
    #[test]
    fn reask_note_from_three_names_all_three_changes() {
        assert_eq!(
            reask_note(3),
            Some(
                "Asking again because what is collected has changed: Crash reports can now \
                 include a playback error report with its steps. Crash reports can now include \
                 a sign-in error report. Reports can now say how your sign-in is stored."
            )
        );
    }

    /// Every version from 4 up to the current policy has a row in [`REASK_CHANGES`] — so a future
    /// bump with no row fails HERE rather than silently producing a bare prefix or a note that
    /// skips a version.
    #[test]
    fn every_reasked_version_has_a_row() {
        for v in 4..=POLICY_VERSION {
            assert!(
                REASK_CHANGES.iter().any(|&(version, _)| version == v),
                "version {v} bumped POLICY_VERSION but has no REASK_CHANGES row"
            );
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
