//! **Telemetry: the decision, the spool, and the worker that drains it.**
//!
//! The pieces have one ordering. [`consent`] and its storage are the part that has to be right
//! before anything can be collected or sent and are answerable entirely on the host. [`sentry`]
//! and [`posthog`] are the wire FORMATS; [`playback`] is the closed handled-error schema. Their
//! failures are silent 400s from a server that explains nothing, so they are pinned to tests while
//! there is still no network to hide behind. [`queue`] is the framing and caps, pure; [`spool`] is
//! the file those bytes live in and the one owner every read and write goes through; [`sender`] is
//! the socket, and the place the credential split decides which project this build reports to.
//!
//! **Ungated**, like `diag::scrub` and `diag::schema`, and for the reason both of those record: the
//! guarantees here are the tests — that no identifier exists before an opt-in, that withdrawal
//! destroys what it withdrew, that the event path fails closed on an ANSWERED "no" (an unanswered
//! decision defers the sign-in family instead, bounded, in memory — see `diag::defer`), that a
//! record queued while a flush was on the network is not erased by that flush's commit — and a
//! test behind a feature the default gate does not build is a test that never runs.
pub(crate) mod consent;
pub(crate) mod crashreport;
pub(crate) mod native;
pub(crate) mod oneoff;
pub(crate) mod playback;
pub(crate) mod posthog;
pub(crate) mod queue;
pub(crate) mod sender;
pub(crate) mod sentry;
pub(crate) mod signin;
pub(crate) mod spool;
pub(crate) mod storage;

use consent::Consent;

/// Load the stored decision and publish it for the event path.
///
/// Called once at boot, before anything can report. A missing or unparsable file is the DEFAULT
/// decision — everything off, unanswered — which is the only safe reading: a file we cannot
/// understand is not consent.
pub(crate) fn boot() -> native::Guard {
    let c = load();
    // Logged because the alternative is a silent behavioural difference between two televisions.
    // No identifier in the line: it is the one field here worth not putting in a log that gets
    // pasted into issue threads, and its PRESENCE is the only fact worth stating anyway.
    let presence = |id: &Option<String>| if id.is_some() { "yes" } else { "none" };
    crate::log(&format!(
        "telemetry: answered={} errors={} usage={} id={} errors_id={}",
        c.answered(),
        c.errors,
        c.usage,
        presence(&c.install_id),
        presence(&c.errors_id)
    ));
    consent::install(c.clone());
    if !c.errors {
        crate::player::report::clear_error_trace();
    }
    // **Which destinations this build can actually reach**, once, at boot. A decision of `usage=true`
    // in a build with no PostHog key sends nothing, and every other line in this log looks
    // identical either way — `diag::event` returns before the queue, correctly and silently. This
    // is the line that says whether telemetry is WIRED, as against merely consented to, and it
    // names no endpoint: which projects those are is a release-audit fact, not a per-boot one.
    crate::log(&format!(
        "telemetry: env={} sentry={} posthog={}",
        sender::ENVIRONMENT,
        if sender::has_sentry() { "yes" } else { "no" },
        if sender::has_posthog() { "yes" } else { "no" }
    ));
    // **After the install, and before anything in this process can fault.** The records being read
    // were written by a process that no longer exists — that is the whole reason the crash log is
    // on disk — so this is the only moment they can be turned into reports. It queues; it does not
    // send. The flush is spawned later, after `net::global_init`, which is a separate ordering
    // constraint that has already been got wrong once: a boot flush ahead of it logged
    // `holding 5 records` directly above `net: bound libcurl`.
    // Queue a completed out-of-process event first. The local C/panic log may describe the same
    // crash; report_pending consumes the native keys so one process death remains one Sentry event.
    let native_crashes = native::import_pending();
    crashreport::report_pending(&native_crashes);
    // The SDK capture backend starts only after consent is published and old fallback records are
    // safely queued. Its guard lives for the whole app and restores the C tracer on clean exit.
    native::sync(&c)
}

/// The first candidate that exists and parses. Same search-order shape as the session file, and
/// for the same reason: which of the two `/media` directories is writable depends on the jail
/// profile, so the answer cannot be a literal.
fn load() -> Consent {
    load_from(&candidates())
}

/// Where the decision lives: `paths::telemetry_candidates()`, until a test redirects it to a file
/// of its own. Same shape and same reason as `session::redirect_for_test`: every real candidate is
/// either a device path that does not exist on the dev Mac or the directory the test binary runs
/// from, so a test that writes through the real list leaves a consent file in `target/`.
#[cfg(not(test))]
fn candidates() -> Vec<std::path::PathBuf> {
    crate::paths::telemetry_candidates()
}

#[cfg(test)]
static TEST_FILE: std::sync::Mutex<Option<std::path::PathBuf>> = std::sync::Mutex::new(None);

#[cfg(test)]
fn candidates() -> Vec<std::path::PathBuf> {
    match TEST_FILE.lock().unwrap_or_else(|e| e.into_inner()).clone() {
        Some(p) => vec![p],
        None => crate::paths::telemetry_candidates(),
    }
}

/// Point this module's decision file at `p`, or back at the real search order with `None`. The
/// caller holds `crate::testlock::serial()` for the whole test: this is a crate global.
#[cfg(test)]
pub(crate) fn redirect_for_test(p: Option<std::path::PathBuf>) {
    *TEST_FILE.lock().unwrap_or_else(|e| e.into_inner()) = p;
}

fn load_from(candidates: &[std::path::PathBuf]) -> Consent {
    for p in candidates {
        let Some((bytes, trust)) = crate::plex::session::read_owned_regular_trusted(p) else {
            continue;
        };
        if !trust.content_trusted() {
            // Write-widened: another uid could have rewritten this file, so a stored `usage: true`
            // or `errors: true` here is not provably this person's decision, and its ids are not
            // provably ours either — a forged consent file must not silently stand in for one. Both
            // categories go back to unanswered (the question is asked again on the next
            // opportunity, same as a fresh install), the identifiers are discarded with it, and the
            // reset state is written back at 0600 so the next boot does not repeat this discovery.
            crate::log(
                "telemetry: consent file was writable by others — content is untrusted, resetting",
            );
            let reset = Consent::default();
            if let Ok(json) = serde_json::to_vec_pretty(&reset) {
                let _ = crate::plex::session::write_atomic(p, &json);
            }
            // Every other path that revokes a decision purges the spool of records queued under
            // it (`record`'s own call, right below) — a forged consent file is exactly such a
            // revocation, and without this a record queued under the untrusted decision would
            // still leave the device under it (review finding, 2026-09-10).
            spool::purge_withdrawn(&reset);
            return reset;
        }
        if let Ok(c) = serde_json::from_slice::<Consent>(&bytes) {
            return consent::migrate_loaded(c);
        }
    }
    Consent::default()
}

/// Record a decision: write it, then publish it. **Write first** — a decision that took effect but
/// did not persist would silently re-ask on the next boot while having already acted on itself.
///
/// A total write failure is logged and still applied to this session. The alternative is refusing
/// to honour something a person just chose because a disk is full, which is worse in both
/// directions: it ignores a "no", and it ignores a "yes".
pub(crate) fn record(c: Consent) {
    let enabling_errors = newly_enables_errors(consent::current().as_ref(), &c);
    let Ok(json) = serde_json::to_vec_pretty(&c) else {
        return;
    };
    let stored = candidates()
        .iter()
        .any(|p| crate::plex::session::write_atomic(p, &json));
    if !stored {
        crate::log("telemetry: could not persist the decision to ANY candidate path");
    }
    // Consent is prospective: crash diagnostics accumulated while this switch was off stay local.
    // Do this before publishing `c`, so there is no interval in which an old record can be read as
    // newly authorised.
    if enabling_errors {
        crashreport::discard_pending_before_opt_in();
        // Issue #76 review (should-fix): a stage `plex::session::report_once` genuinely DROPPED
        // (a real "No", not merely "not asked yet") must get its one attempt back the moment this
        // same process turns Errors on — see `retry_dropped_stages`'s own doc.
        crate::plex::session::retry_dropped_stages();
        // Its twin for the sign-in save's own report, which is deduped on a (save outcome,
        // preserve reason) pair rather than on a stage — see `storage::report_sign_in_not_persisted`.
        storage::retry_dropped_sign_in();
    }
    consent::install(c.clone());
    // Issue #75: replay whatever sign-in funnel `diag::event` had to hold back because the consent
    // question was still unanswered when it happened. Unconditional — a "yes" lets the replay
    // through the normal gate, a "no" hits that same gate and drops, and either way the queue must
    // not survive to be misread by the next decision.
    crate::diag::replay_deferred();
    // Issue #76: the storage-error report's own twin — a locked/refused read found before the
    // Errors channel's consent question (or its scope-6 extension) was answered waits here rather
    // than being dropped for good. Same unconditional shape: a "yes" sends it through the normal
    // gate, a "no" hits that gate and drops, and the queue empties either way.
    storage::replay_deferred();
    if !c.errors {
        crate::player::report::clear_error_trace();
    }
    // Install first, then purge. A record queued between the two would be one the new decision
    // already governs, so it is caught by the next flush's per-record check; the other order leaves
    // a window in which a record of a just-withdrawn category is written by a path still reading
    // the old consent and then never looked at again.
    spool::purge_withdrawn(&c);
    native::sync_change(&c);
}

/// **End the signed-in account's tenure over telemetry.** The decision returns to *unanswered*
/// and is PUBLISHED FIRST — before any file I/O — so from that instant every STANDING producer's
/// gate answers no and the sender's per-record revision check picks up no further standing
/// record; this is what lets `PRIVACY.md` say that no further report of a category is picked up
/// after a sign-out. ONE record the sender had already passed through that check may still go
/// out — the check-to-POST gap has no cancellation — and the policy says exactly that, no more.
/// The one-off sign-in report is not a standing producer and answers no gate at all; what actually
/// removes it here is the purge below, same as everything else queued. Then both identifiers are
/// gone with it, the consent file
/// is unlinked from every candidate location — a candidate that cannot be unlinked is overwritten
/// with the default decision, and one that refuses both is logged: that disk is also refusing the
/// session's own clear, the same failure sign-out already has for the credentials — every queued
/// record, standing or one-off, is purged (`spool::purge_all_local`), and the native capture
/// backend is stopped with its pending envelopes removed.
/// Nothing is written otherwise: a missing file IS "never asked", which is what Delete all local
/// data needs the name to mean.
///
/// ONE mechanism with two consumers, both behind `auth::forget_account`: the two Sign out doors
/// (account menu, who's-watching pill) and Delete all local data.
///
/// Why sign-out ends it: consent is given by the person who signed the television in, and it
/// authorises nothing about the next person to sign in through the QR flow. Until 2026-09-04 the
/// decision and both identifiers deliberately outlived the sign-in ("so that a decision you have
/// already made is not put to you again"), which meant account B was never asked and every report
/// B caused went out under A's consent and A's identifiers. A managed-profile switch is not a
/// sign-out and keeps the decision; an uninstall keeps the sign-in, so it keeps the decision too.
///
/// The crash MARK (`paths::telemetry_crashmark_candidates`) is deliberately left alone: it records
/// how much of the crash log has been read — a fact about the log, not about anybody — and the
/// next opt-in watermarks the log again through `crashreport::discard_pending_before_opt_in`.
pub(crate) fn forget() {
    let c = Consent::default();
    // Install first — then the files, then the purge: the same order and the same reason as
    // `record`, and here also the moment the sender stops starting requests.
    consent::install(c.clone());
    crate::player::report::clear_error_trace();
    // Issue #76's report lane: what the signed-out account's own sign-in save did with its session
    // file is a fact ABOUT that account, and belongs to it — the same lifetime the consent decision
    // and the crash-report id already have here. See `signin::forget_storage_outcome`.
    signin::forget_storage_outcome();
    oneoff::forget();
    storage::forget();
    for p in candidates() {
        match std::fs::remove_file(&p) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                // A file that will not go must at least stop saying yes.
                let overwritten = serde_json::to_vec_pretty(&c)
                    .map(|json| crate::plex::session::write_atomic(&p, &json))
                    .unwrap_or(false);
                crate::log(&format!(
                    "telemetry: sign-out could not unlink the decision ({e}); overwritten={overwritten}"
                ));
            }
        }
    }
    // Not `purge_withdrawn` — a queued `Category::OneOff` record survives THAT on purpose (see its
    // doc), but sign-out and Delete all local data are an erasure of this television's local
    // state, not a withdrawal of the single press that queued a one-off report, and PRIVACY.md
    // promises both remove "any queued report".
    spool::purge_all_local();
    native::sync_change(&c);
}

fn newly_enables_errors(previous: Option<&Consent>, next: &Consent) -> bool {
    let effectively_allows_errors = |c: &Consent| c.answered() && c.errors;
    effectively_allows_errors(next) && previous.is_none_or(|c| !effectively_allows_errors(c))
}

// ---- the spool, and the one worker that drains it ---------------------------------------------

/// Guards against two flushes at once. A spool is a read-modify-write of one file, so two workers
/// racing would have the second write back a list that does not know what the first acknowledged —
/// re-sending records that were accepted, which is the duplicate-issue failure `event_id` reuse
/// exists to prevent, arriving by a different door.
static FLUSHING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// At most one sleeping retry worker. Manual flushes may happen while it waits; the eventual wake
/// is harmless, while spawning one sleeper per flush would consume this small device's thread cap.
static RETRY_SCHEDULED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Drain the spool on a worker thread.
///
/// **Never the main loop and never signal context.** A flush opens a socket and can block for
/// [`sender`]'s whole timeout; on the main thread that is a visibly frozen interface, and from a
/// signal handler it is neither async-signal-safe nor able to finish.
///
/// Returns immediately. A refused spawn is a return value rather than a panic — `task::spawn_small`
/// exists because `thread::spawn` panics on EAGAIN and killed this app once.
pub(crate) fn flush_soon() {
    // Unit tests may deliberately configure the DEV endpoints through Makefile environment
    // variables, but their spool path is a process-global test seam. A worker that outlives the
    // serial test guard can then wake in a later test and drain that later test's fixture. Tests
    // exercise `flush_now` synchronously where needed; never launch a background sender against a
    // movable test path. Shipping builds retain the asynchronous flush behavior below.
    if cfg!(test) {
        return;
    }

    use std::sync::atomic::Ordering;
    if !sender::configured() {
        return; // nothing in this build to send to — see `sender`'s module doc
    }
    // A decision must EXIST — nothing loaded means nothing consented.
    let Some(c) = consent::current() else { return };
    // …but deliberately no `if !c.any() { return }`. A withdrawal is exactly when the spool most
    // needs draining: `flush_now` retires a record whose category is now off without sending it,
    // so the purge rides the same path instead of needing one of its own. Returning early here
    // would leave records on disk that nobody has consented to, which is the opposite of what a
    // withdrawal is for. With both switches off the flush loads the spool, retires everything and
    // writes back an empty file, sending nothing.
    let _ = c.any();
    let decision_revision = consent::revision();
    if FLUSHING.swap(true, Ordering::AcqRel) {
        return; // one at a time — see FLUSHING
    }
    let ok = crate::task::spawn_small("telemetry", move || {
        let retry = flush_now(&c, decision_revision);
        FLUSHING.store(false, Ordering::Release);
        if let Some(seconds) = retry {
            if RETRY_SCHEDULED.swap(true, Ordering::AcqRel) {
                return;
            }
            // A retry is an actual schedule, not merely a number in a log. This worker owns no
            // spool lock and has a small stack; when it wakes it goes through FLUSHING again.
            let scheduled = crate::task::spawn_small("telemetry-retry", move || {
                std::thread::sleep(std::time::Duration::from_secs(seconds));
                RETRY_SCHEDULED.store(false, Ordering::Release);
                flush_soon();
            });
            if !scheduled {
                RETRY_SCHEDULED.store(false, Ordering::Release);
            }
        }
    });
    if !ok {
        FLUSHING.store(false, Ordering::Release);
    }
}

/// The flush itself, on the worker.
fn flush_now(c: &consent::Consent, decision_revision: u32) -> Option<u64> {
    let all = spool::read();
    if all.is_empty() {
        return None;
    }
    // Records that leave the spool, whether because a server took them or because nobody consents
    // to them any more. One list, because `queue::ack` asks one question — is this record still
    // ours to keep — and the two reasons for "no" need no distinction downstream.
    let mut retired: Vec<String> = Vec::new();
    let (newly_retired, dropped_by_consent, retry) = process_records(
        &all,
        c,
        || consent::revision() == decision_revision,
        sender::send_one,
    );
    retired.extend(newly_retired);
    if let Some(s) = retry {
        crate::log(&format!(
            "telemetry: holding {} records, ~{s}s",
            all.len() - retired.len()
        ));
    }
    // **Records queued under an earlier decision, re-checked against the CURRENT one at flush
    // time.** `sender::allowed` already answers this per record above (`process_records`); the log
    // line is what the 2026-09-10 TV telemetry proof's negative control had to work around by
    // moving a stale spool aside by hand instead of being able to read that they were dropped.
    if dropped_by_consent > 0 {
        crate::log(&format!(
            "telemetry: dropped {dropped_by_consent} spooled record(s) the current consent no longer allows"
        ));
    }
    if !retired.is_empty() {
        spool::commit_retiring(&retired);
        crate::log(&format!(
            "telemetry: flushed {} of {} record(s)",
            retired.len(),
            all.len()
        ));
    }
    retry
}

/// Process each destination as an independent logical lane. A dead/rate-limited Sentry endpoint
/// cannot prevent a later PostHog record from being attempted, or vice versa.
/// Returns `(retired ids, how many of those were dropped rather than sent because the CURRENT
/// consent no longer allows their category, the retry hold)`. The middle value is what lets the
/// flush log a consent-drop distinctly from an ordinary send/hopeless retirement — see
/// `flush_now`'s doc and the 2026-09-10 TV telemetry proof's negative control, which had no way to
/// read that a stale spooled record would be dropped rather than sent on the next flush.
fn process_records(
    all: &[queue::Record],
    c: &consent::Consent,
    mut still_current: impl FnMut() -> bool,
    mut send: impl FnMut(&queue::Record) -> (sender::Verdict, Option<u64>),
) -> (Vec<String>, usize, Option<u64>) {
    let mut retired = Vec::new();
    let mut dropped_by_consent = 0usize;
    let mut retry: Option<u64> = None;
    'destinations: for dest in [queue::Dest::Sentry, queue::Dest::PostHog] {
        for r in all.iter().filter(|r| r.dest == dest) {
            // Never carry an old consent snapshot through a withdrawal, a sign-out or a quick
            // off→on cycle. A request that already passed this check may finish because the socket
            // API has no cancellation; PRIVACY.md ("Your choices") states that narrow in-flight
            // boundary explicitly — it did not until 2026-09-04, while this comment said it did.
            if !still_current() {
                break 'destinations;
            }
            // Per record, against its own category — a spool written before a withdrawal (or
            // inherited from disk under a decision this process never lived through as a `record`
            // call, so `spool::purge_withdrawn` never ran against it) can still hold records of a
            // category that is now off. `r.category == OneOff` always answers true here — see
            // `sender::allowed`'s doc — so a one-off report is never counted as a consent-drop.
            if !sender::allowed(r, c) {
                retired.push(r.event_id.clone());
                dropped_by_consent += 1;
                continue;
            }
            match send(r) {
                (sender::Verdict::Done, _) | (sender::Verdict::Hopeless, _) => {
                    retired.push(r.event_id.clone())
                }
                // Stop this lane only. The failure applies to later records for the same endpoint,
                // but says nothing about the independent service in the other lane.
                (sender::Verdict::Keep, hold) => {
                    let s = hold.unwrap_or(sender::DEFAULT_HOLD_S);
                    retry = Some(retry.map_or(s, |old| old.min(s)));
                    break;
                }
            }
        }
    }
    (retired, dropped_by_consent, retry)
}

/// 16 bytes of `/dev/urandom` as lowercase hex — the ONLY way a consent identifier (the analytics
/// `install_id` or the crash-report `errors_id`) is ever produced.
///
/// Reads the device directly rather than taking a dependency: this crate has no RNG, and the one
/// property that matters is that the value is not derived from anything about this television or
/// this account. A read failure yields `None`, and [`consent::apply`] then records that channel as
/// off rather than inventing a fallback — a "random" identifier built from a clock or a MAC is
/// exactly the identifier this design refuses.
pub(crate) fn mint_id() -> Option<String> {
    let mut buf = [0u8; 16];
    use std::io::Read;
    std::fs::File::open("/dev/urandom")
        .ok()?
        .read_exact(&mut buf)
        .ok()?;
    Some(buf.iter().map(|b| format!("{b:02x}")).collect())
}

/// Does `s` have the shape [`mint_id`] produces — 32 lowercase hex characters, nothing else?
///
/// The native importer uses it to decide whether a `user.id` the crash daemon captured is OUR
/// crash-report id or something a future SDK scope put there: anything that is not this shape is
/// dropped with the rest of the user object.
pub(crate) fn is_minted_id(s: &str) -> bool {
    s.len() == 32
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_errors_off_to_on_transition_discards_preconsent_crashes() {
        let state = |errors| Consent {
            asked_version: consent::POLICY_VERSION,
            errors,
            usage: false,
            install_id: None,
            errors_id: None,
            ..Default::default()
        };
        assert!(newly_enables_errors(None, &state(true)));
        assert!(newly_enables_errors(Some(&state(false)), &state(true)));
        assert!(!newly_enables_errors(Some(&state(true)), &state(true)));
        assert!(!newly_enables_errors(Some(&state(true)), &state(false)));

        let stale_yes = Consent {
            asked_version: consent::POLICY_VERSION.saturating_sub(1),
            errors: true,
            usage: false,
            install_id: None,
            errors_id: None,
            ..Default::default()
        };
        assert!(
            newly_enables_errors(Some(&stale_yes), &state(true)),
            "a stale-policy boolean is not current authorization; the new answer must watermark crashes accumulated while consent failed closed",
        );
    }

    #[test]
    fn a_held_destination_does_not_block_the_other_destination() {
        let record = |id: &str, category, dest| queue::Record {
            category,
            dest,
            event_id: id.into(),
            body: b"{}".to_vec(),
        };
        let all = vec![
            record("s1", queue::Category::Errors, queue::Dest::Sentry),
            record("p1", queue::Category::Usage, queue::Dest::PostHog),
        ];
        let c = consent::Consent {
            asked_version: consent::POLICY_VERSION,
            errors: true,
            usage: true,
            install_id: Some("id".into()),
            errors_id: Some("eid".into()),
            ..Default::default()
        };
        let mut attempted = Vec::new();
        let (retired, dropped_by_consent, retry) = process_records(
            &all,
            &c,
            || true,
            |r| {
                attempted.push(r.event_id.clone());
                if r.dest == queue::Dest::Sentry {
                    (sender::Verdict::Keep, Some(7))
                } else {
                    (sender::Verdict::Done, None)
                }
            },
        );
        assert_eq!(attempted, vec!["s1", "p1"]);
        assert_eq!(retired, vec!["p1"]);
        assert_eq!(dropped_by_consent, 0);
        assert_eq!(retry, Some(7));
    }

    /// **The flush re-checks consent, not just append time.** The 2026-09-10 TV telemetry proof
    /// had to move a stale spool aside by hand for its negative control — three records queued by
    /// an EARLIER session sat in the spool under a decision that no longer covered them, and
    /// nothing in the flush path proved they would be dropped rather than sent on the next flush.
    /// A record whose CURRENT consent no longer allows its category must never reach `send`, must
    /// be retired (removed from the spool) anyway, and must be logged as a consent-drop distinct
    /// from an ordinary send.
    #[test]
    fn process_records_drops_a_record_the_current_consent_no_longer_allows() {
        let record = |id: &str, category, dest| queue::Record {
            category,
            dest,
            event_id: id.into(),
            body: b"{}".to_vec(),
        };
        let all = vec![record(
            "stale-error",
            queue::Category::Errors,
            queue::Dest::Sentry,
        )];
        let no_consent = consent::Consent {
            asked_version: consent::POLICY_VERSION,
            errors: false,
            usage: false,
            ..Default::default()
        };
        let mut attempted = Vec::new();
        let (retired, dropped_by_consent, retry) = process_records(
            &all,
            &no_consent,
            || true,
            |r| {
                attempted.push(r.event_id.clone());
                (sender::Verdict::Done, None)
            },
        );
        assert!(attempted.is_empty(), "a disallowed record must never reach send");
        assert_eq!(retired, vec!["stale-error"]);
        assert_eq!(dropped_by_consent, 1);
        assert_eq!(retry, None);
    }

    /// **End-to-end through `flush_now` and the real spool**: a record queued while consent
    /// allowed it, flushed once the CURRENT consent no longer does, leaves the spool empty and
    /// logs the drop — the exact scenario the negative control worked around by hand. Mirrors
    /// `spool::tests::a_withdrawal_purges_its_own_category_and_leaves_the_other` in spirit, but at
    /// the FLUSH boundary rather than the withdrawal-time purge, which is a separate, already-
    /// covered mechanism (`record`'s call to `spool::purge_withdrawn`).
    #[test]
    fn flush_drops_a_spooled_record_the_current_consent_no_longer_allows() {
        let _g = crate::testlock::serial();
        let dir =
            std::env::temp_dir().join(format!("plxnative-flush-consent-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let saved = consent::current();
        spool::set_test_path(Some(dir.join("spool.bin")));

        spool::append(&queue::Record {
            category: queue::Category::Errors,
            dest: queue::Dest::Sentry,
            event_id: "stale-error".into(),
            body: b"{}".to_vec(),
        });
        assert_eq!(spool::read().len(), 1, "the record must be on disk before the flush");

        let no_consent = consent::Consent {
            asked_version: consent::POLICY_VERSION,
            errors: false,
            usage: false,
            ..Default::default()
        };
        consent::install(no_consent.clone());

        let logged = crate::with_test_log(|p| {
            let retry = flush_now(&no_consent, consent::revision());
            assert!(retry.is_none());
            assert!(
                spool::read().is_empty(),
                "a record the current consent no longer allows must not survive the flush"
            );
            std::fs::read_to_string(p).unwrap_or_default()
        });
        assert!(
            logged.contains(
                "telemetry: dropped 1 spooled record(s) the current consent no longer allows"
            ),
            "missing the consent-drop log line: {logged}"
        );

        spool::set_test_path(None);
        if let Some(c) = saved {
            consent::install(c);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The identifier is 32 hex characters and two mints differ. Not a randomness test — it is a
    /// test that the SOURCE is the device and not a constant, which is the failure that would make
    /// every install share one id and nobody notice.
    #[test]
    fn a_minted_identifier_is_random_hex() {
        let Some(a) = mint_id() else { return }; // no /dev/urandom: nothing to assert
        assert_eq!(a.len(), 32, "16 bytes as hex");
        assert!(a
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        assert!(is_minted_id(&a), "the shape check rejects what the mint produces");
        let b = mint_id().expect("second read");
        assert_ne!(a, b, "two mints produced the same identifier");
    }

    /// The shape check is exact: length, case and alphabet. It is what stands between a future SDK
    /// scope value and the wire, so a near miss must not pass.
    #[test]
    fn the_id_shape_check_is_exact() {
        assert!(is_minted_id(&"0".repeat(32)));
        assert!(is_minted_id("0123456789abcdef0123456789abcdef"));
        for bad in [
            "",
            "0123456789ABCDEF0123456789abcdef",
            "0123456789abcdef0123456789abcde",
            "0123456789abcdef0123456789abcdef0",
            "0123456789abcdef0123456789abcdeg",
            "0123456789abcdef-0123456789abcde",
            "id:0123456789abcdef0123456789abcd",
        ] {
            assert!(!is_minted_id(bad), "accepted {bad:?}");
        }
    }

    /// **Ending the tenure leaves neither identifier in the snapshot and no file on disk.** The
    /// snapshot is the half a producer on the render thread reads, so a report queued after the
    /// sign-out must find nothing to attach; the file is the half the next boot reads.
    /// `auth::tests::signing_out_leaves_no_consent_and_no_identifier_for_the_next_account` grades
    /// the same thing through the real sign-out tail.
    #[test]
    fn forgetting_the_tenure_clears_both_identifiers_and_the_file() {
        /// The redirects and the snapshot, handed back on drop, so a failed assertion cannot leave
        /// the next test writing into this directory.
        struct Redirects {
            dir: std::path::PathBuf,
            saved: Option<Consent>,
        }
        impl Drop for Redirects {
            fn drop(&mut self) {
                spool::set_test_path(None);
                redirect_for_test(None);
                if let Some(c) = self.saved.take() {
                    consent::install(c);
                }
                let _ = std::fs::remove_dir_all(&self.dir);
            }
        }
        let _g = crate::testlock::serial();
        let dir = std::env::temp_dir().join(format!("plxnative-forget-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let _redirects = Redirects {
            dir: dir.clone(),
            saved: consent::current(),
        };
        let file = dir.join("telemetry.json");
        redirect_for_test(Some(file.clone()));
        spool::set_test_path(Some(dir.join("spool.jsonl")));
        record(consent::apply(&Consent::default(), true, true, || {
            Some("f".repeat(32))
        }));
        assert!(file.exists());
        assert!(consent::errors_id().is_some() && consent::allows_usage());
        forget();
        let after = consent::current().expect("a default decision is published, not none");
        assert!(!after.any() && !after.answered());
        assert!(after.install_id.is_none() && after.errors_id.is_none());
        assert!(consent::errors_id().is_none() && !consent::allows_errors());
        assert!(!file.exists(), "the decision file survived");
    }

    /// An unreadable or corrupt file is the DEFAULT decision, never a partial one — a file we
    /// cannot understand is not consent.
    #[test]
    fn an_unparsable_file_is_not_consent() {
        let c: Consent = serde_json::from_slice(b"{ not json").unwrap_or_default();
        assert!(!c.any() && !c.answered());
    }

    #[test]
    fn a_symlink_cannot_supply_telemetry_consent() {
        use std::os::unix::fs::symlink;
        let _g = crate::testlock::serial();
        let dir =
            std::env::temp_dir().join(format!("plxnative-consent-symlink-{}", std::process::id()));
        let _ = std::fs::create_dir(&dir);
        let victim = dir.join("attacker.json");
        let candidate = dir.join("consent.json");
        let _ = std::fs::remove_file(&candidate);
        std::fs::write(
            &victim,
            format!(
                r#"{{"asked_version":{},"errors":true,"usage":true}}"#,
                consent::POLICY_VERSION
            ),
        )
        .unwrap();
        symlink(&victim, &candidate).unwrap();

        let loaded = load_from(&[candidate.clone()]);
        assert!(!loaded.any() && !loaded.answered());

        let _ = std::fs::remove_file(candidate);
        let _ = std::fs::remove_file(victim);
        let _ = std::fs::remove_dir(dir);
    }

    /// **A write-widened consent file is not this person's decision — it is discarded like a
    /// symlink or a corrupt file, not merely repaired.** Seeded here as a stored "yes/yes" (the
    /// worst case: a forged `usage: true`/`errors: true` reaching further consent than anyone here
    /// actually granted). Read-only widening (`0o644`) is a disclosure problem and is covered
    /// separately below — this test is specifically the write-widened (`0o666`) case.
    #[test]
    fn a_write_widened_consent_file_is_treated_as_unanswered_and_reset_to_0600() {
        use std::os::unix::fs::PermissionsExt;
        let _g = crate::testlock::serial();
        let dir = std::env::temp_dir()
            .join(format!("plxnative-consent-widened-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("telemetry.json");
        std::fs::write(
            &file,
            format!(
                r#"{{"asked_version":{},"errors":true,"usage":true,"install_id":"{}","errors_id":"{}"}}"#,
                consent::POLICY_VERSION,
                "a".repeat(32),
                "b".repeat(32)
            ),
        )
        .unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o666)).unwrap();

        let loaded = load_from(&[file.clone()]);
        assert!(
            !loaded.errors && !loaded.usage,
            "both categories must come back unanswered, not the forged yes/yes"
        );
        assert!(!loaded.answered() && !loaded.any());
        assert!(
            loaded.install_id.is_none() && loaded.errors_id.is_none(),
            "a forged file's identifiers are not ours and must be discarded with it"
        );

        let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "the reset decision must still be written 0600: {mode:o}");
        let on_disk: Consent = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        assert!(!on_disk.answered() && !on_disk.any(), "the reset state must be persisted");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The read-only-widened twin: `0o644` carries no write bit, so the stored decision is still
    /// provably this person's own and must load exactly as written.
    #[test]
    fn a_read_only_widened_consent_file_still_loads_as_written() {
        use std::os::unix::fs::PermissionsExt;
        let _g = crate::testlock::serial();
        let dir = std::env::temp_dir()
            .join(format!("plxnative-consent-readable-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("telemetry.json");
        std::fs::write(
            &file,
            format!(r#"{{"asked_version":{},"errors":true,"usage":false}}"#, consent::POLICY_VERSION),
        )
        .unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();

        let loaded = load_from(&[file.clone()]);
        assert!(loaded.errors && !loaded.usage && loaded.answered());

        let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "the widened mode was not repaired: {mode:o}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **Same known limitation as `plex::session`'s replay test.** Ownership + mode checks (and the
    /// trust-vs-repair split) tell a forged consent file apart from ours; they cannot tell a
    /// STALE-but-genuine one apart from the current one. A peer with rename rights in the shared
    /// namespace can move this install's own, currently-valid, correctly-0600 consent file aside,
    /// let a later decision overwrite it, and move the old bytes back. This test PINS that as
    /// expected (green) behavior, not a bug to fix: the replayed older decision loads as though it
    /// were current.
    #[test]
    fn a_replayed_older_valid_consent_file_is_accepted() {
        let _g = crate::testlock::serial();
        let dir =
            std::env::temp_dir().join(format!("plxnative-consent-replay-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("telemetry.json");

        let old = format!(r#"{{"asked_version":{},"errors":false,"usage":false}}"#, consent::POLICY_VERSION);
        std::fs::write(&file, &old).unwrap();
        let old_bytes = std::fs::read(&file).unwrap();

        // A later decision supersedes it.
        std::fs::write(
            &file,
            format!(r#"{{"asked_version":{},"errors":true,"usage":true}}"#, consent::POLICY_VERSION),
        )
        .unwrap();
        let current = load_from(&[file.clone()]);
        assert!(current.errors && current.usage);

        // The replay: the old bytes come back, same owner, same mode (an existing path's mode is
        // untouched by an ordinary overwrite).
        std::fs::write(&file, &old_bytes).unwrap();

        let replayed = load_from(&[file.clone()]);
        assert!(
            !replayed.errors && !replayed.usage,
            "the replayed OLDER decision is indistinguishable from a current one — known limitation"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A file written by a FUTURE build, carrying fields this one does not know, still parses —
    /// and a file missing fields still parses. Both matter on a device that can be downgraded by a
    /// reinstall while the file survives it.
    #[test]
    fn the_stored_shape_tolerates_version_skew() {
        let older: Consent = serde_json::from_slice(br#"{"asked_version":1,"usage":true}"#)
            .expect("a file with fewer fields still parses");
        assert!(older.usage && !older.errors && older.install_id.is_none());
        let newer: Consent =
            serde_json::from_slice(br#"{"asked_version":1,"usage":true,"a_field_from_later":7}"#)
                .expect("a file with more fields still parses");
        assert!(newer.usage);
    }
}
