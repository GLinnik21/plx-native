//! **Telemetry: the decision, the spool, and the worker that drains it.**
//!
//! The pieces have one ordering. [`consent`] and its storage are the part that has to be right
//! before anything can be collected or sent and are answerable entirely on the host. [`sentry`]
//! and [`posthog`] are the wire FORMATS; [`playback`] is the closed handled-error schema. Their
//! failures are silent 400s from a server that explains nothing, so they are pinned to tests while
//! there is still no network to hide behind. [`queue`] is the framing and caps, pure; [`spool`] is
//! the file those bytes live in and the one owner every read and write goes through; [`sender`] is
//! the socket, and the place the credential split decides which project this build reports to.
//! [`incident`] is the closed onboarding-failure schema, sent either as a standing report or — to
//! somebody who was never asked, or said Yes before it existed — as a one-off on an explicit press,
//! whose bounded transport is [`oneoff`]. Either one's Report ID is watched in [`delivery`], which
//! the flush, the spool and the one-off fallback all settle, so the screen can say "sent" only once
//! a server accepted it. [`classes`] is the closed vocabulary [`playback`] serialises, owned here so
//! the player classifies INTO it rather than this layer naming the player; [`transition`] is what a
//! consent change does to the resources this module keeps.
//!
//! **Ungated**, like `eventlog::scrub` and `diag::schema`, and for the reason both of those record: the
//! guarantees here are the tests — that no identifier exists before an opt-in, that withdrawal
//! destroys what it withdrew, that the event path fails closed, that a record queued while a flush
//! was on the network is not erased by that flush's commit — and a test behind a feature the
//! default gate does not build is a test that never runs.
pub mod classes;
pub mod consent;
pub mod crashreport;
pub mod delivery;
pub mod incident;
pub mod oneoff;
pub mod persistence;
pub mod native;
pub mod window;
pub mod playback;
pub mod posthog;
pub mod queue;
pub mod sender;
pub mod sentry;
pub mod spool;
pub mod transition;

/// The prefix of the Sentry `release` string, and of the one handed to [`set_release`].
const RELEASE_PREFIX: &str = "plxnative@";

/// The release this build reports, `plxnative@<version>`, handed in by the application.
static RELEASE: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();

/// Record the release string every outbound report carries (Sentry `release`, the SDK and client
/// version, the usage context's `app_version`).
///
/// **The application hands it in**: `PLX_VERSION` is published by the application's build script, a
/// `cargo:rustc-env` reaches that one crate only, and the rule that derives it is written once
/// (`build.rs`, `ci/version_rule.py`). The application passes `concat!("plxnative@",
/// env!("PLX_VERSION"))`, composed at compile time in ITS crate, because `ci/check-package.py`
/// grades the packaged binary for exactly that contiguous string; a `format!` here would leave the
/// prefix and the number in different places. It is the second statement of `enter_application`,
/// right behind `plex::identity::set_version`, which `app::boot::seam_order_tests` pins. Idempotent
/// for the same string; a different one is a wiring bug and is refused in a debug build.
pub fn set_release(release: &'static str) {
    debug_assert!(release.starts_with(RELEASE_PREFIX), "telemetry::set_release takes `plxnative@<version>`");
    let kept = RELEASE.get_or_init(|| release);
    debug_assert_eq!(*kept, release, "telemetry::set_release called with two releases");
}

/// The Sentry `release` string, `plxnative@<version>`.
///
/// Read before [`set_release`], a shipping build PANICS rather than label a report with a version no
/// release had (the television's build is `--release`, where a `debug_assert!` is compiled out);
/// a `test-support` build answers `plxnative@0.0.0-test`.
pub fn release() -> &'static str {
    match RELEASE.get() {
        Some(r) => r,
        None => {
            #[cfg(any(test, feature = "test-support"))]
            {
                "plxnative@0.0.0-test"
            }
            #[cfg(not(any(test, feature = "test-support")))]
            {
                panic!("telemetry::release read before set_release: no report may carry an unset release")
            }
        }
    }
}

/// The app version alone: [`release`] without its prefix.
pub fn app_version() -> &'static str {
    release().strip_prefix(RELEASE_PREFIX).unwrap_or_else(|| release())
}

use consent::Consent;

/// Load the stored decision and publish it for the event path.
///
/// Called once at boot, before anything can report. A missing or unparsable file is the DEFAULT
/// decision — everything off, unanswered — which is the only safe reading: a file we cannot
/// understand is not consent.
pub fn boot() -> native::Guard {
    activate_initial(load())
}

/// Resource activation after controlled initial capture. Same live policy/order as boot.
pub fn activate_initial(c: Consent) -> native::Guard {
    // Logged because the alternative is a silent behavioural difference between two televisions.
    // No identifier in the line: it is the one field here worth not putting in a log that gets
    // pasted into issue threads, and its PRESENCE is the only fact worth stating anyway.
    let presence = |id: &Option<String>| if id.is_some() { "yes" } else { "none" };
    plx_base::eventlog::log(&format!(
        "telemetry: answered={} errors={} usage={} id={} errors_id={}",
        c.answered(),
        c.errors,
        c.usage,
        presence(&c.install_id),
        presence(&c.errors_id)
    ));
    consent::install(c.clone());
    if !c.errors {
        playback::clear_error_trace();
    }
    // **Which destinations this build can actually reach**, once, at boot. A decision of `usage=true`
    // in a build with no PostHog key sends nothing, and every other line in this log looks
    // identical either way — `diag::event` returns before the queue, correctly and silently. This
    // is the line that says whether telemetry is WIRED, as against merely consented to, and it
    // names no endpoint: which projects those are is a release-audit fact, not a per-boot one.
    plx_base::eventlog::log(&format!(
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
    // The native daemon's envelopes and the local C/panic log are recovered together: the two may
    // describe the same death, and pairing them keeps it one Sentry event — the more useful one.
    crashreport::recover_pending();
    // The SDK capture backend starts only after consent is published and old fallback records are
    // safely queued. Its guard lives for the whole app and restores the C tracer on clean exit.
    native::sync(&c)
}

/// The first candidate that exists and parses. Same search-order shape as the session file, and
/// for the same reason: which of the two `/media` directories is writable depends on the jail
/// profile, so the answer cannot be a literal.
///
/// `plxnative-consentstate` (dev builds) replaces what is stored, for the onboarding-report
/// captures — see `consent::state_override`. Never under test: a stray trigger in
/// the shared runtime directory must not change what a test's redirected file says.
pub fn capture_initial() -> Consent {
    #[cfg(not(any(test, feature = "test-support")))]
    if let Some(c) = consent::state_override() {
        return c;
    }
    load_from(&candidates())
}

fn load() -> Consent { capture_initial() }

/// Where the decision lives: `paths::telemetry_candidates()`, until a test redirects it to a file
/// of its own. Same shape and same reason as `session::redirect_for_test`: every real candidate is
/// either a device path that does not exist on the dev Mac or the directory the test binary runs
/// from, so a test that writes through the real list leaves a consent file in `target/`.
#[cfg(not(any(test, feature = "test-support")))]
fn candidates() -> Vec<std::path::PathBuf> {
    plx_base::paths::telemetry_candidates()
}

#[cfg(any(test, feature = "test-support"))]
static TEST_FILE: std::sync::Mutex<Option<std::path::PathBuf>> = std::sync::Mutex::new(None);

#[cfg(any(test, feature = "test-support"))]
fn candidates() -> Vec<std::path::PathBuf> {
    match TEST_FILE.lock().unwrap_or_else(|e| e.into_inner()).clone() {
        Some(p) => vec![p],
        None => plx_base::paths::telemetry_candidates(),
    }
}

/// Candidate paths used by the live consent resource adapter. Logical owners must use
/// `app::adapters::consent::ConsentAdapter` instead of treating this path inventory as a commit
/// seam.
pub fn resource_candidates() -> Vec<std::path::PathBuf> {
    candidates()
}

/// Point this module's decision file at `p`, or back at the real search order with `None`. The
/// caller holds `plx_base::testlock::serial()` for the whole test: this is a crate global.
#[cfg(any(test, feature = "test-support"))]
pub fn redirect_for_test(p: Option<std::path::PathBuf>) {
    let root = p.as_ref().and_then(|path| path.parent()).map(std::path::Path::to_path_buf);
    *TEST_FILE.lock().unwrap_or_else(|e| e.into_inner()) = p;
    persistence::redirect_root_for_test(root);
}

#[cfg(not(any(test, feature = "test-support")))]
fn load_from(candidates: &[std::path::PathBuf]) -> Consent {
    persistence::load(candidates)
}

/// The canonical record follows a test's scratch candidates, so a decision one test committed
/// cannot become the canonical answer another test's legacy fixture is shadowed by.
#[cfg(any(test, feature = "test-support"))]
fn load_from(candidates: &[std::path::PathBuf]) -> Consent {
    let root = candidates
        .iter()
        .filter_map(|path| path.parent())
        .find(|path| path.exists())
        .map(std::path::Path::to_path_buf);
    persistence::redirect_root_for_test(root);
    persistence::load(candidates)
}

/// Compatibility for resource-focused telemetry and auth tests: the live side effects of a
/// transition, exactly as `app::adapters::consent::ConsentAdapter` performs them — production
/// code has that one explicit commit seam.
#[cfg(any(test, feature = "test-support"))]
pub fn record(next: Consent) {
    let previous = consent::current().unwrap_or_default();
    transition::commit(&previous, &next);
}

/// Test-only twin of [`record`].
#[cfg(any(test, feature = "test-support"))]
pub fn forget() {
    let prior = consent::current().unwrap_or_default();
    transition::forget(&prior);
}

/// Called after the shared account tombstone (`plex::session::clear`'s canonical commit) is
/// confirmed durable. On ARM, `persistence::forget_at` deliberately leaves telemetry/consent's own
/// legacy files in place when a decision is cleared, relying on this sweep to run once the ONE
/// atomic DB8 revocation for both domains — session and telemetry/consent — is confirmed rather
/// than merely queued. Ported from `release/v0.6`'s `telemetry::cleanup_after_account_clear` /
/// `persistence::cleanup_after_combined_clear`, which the 0.7 forward-port dropped along with
/// their only caller (Copilot review on PR #105, finding 7).
#[cfg(all(target_os = "linux", target_arch = "arm", not(feature = "hostsim"), not(any(test, feature = "test-support"))))]
pub fn cleanup_after_account_clear() -> bool {
    persistence::cleanup_after_combined_clear(&candidates()) != persistence::CleanupResult::Failed
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
/// A flush was asked for while one was running. That running flush may have read the spool
/// before the record that asked was appended — a report queued mid-flush would then wait for the
/// next unrelated trigger, its "Sending report…" turning for nothing — so the worker looks again
/// on its way out. Raised BEFORE the `FLUSHING` swap, so a worker finishing between the two
/// cannot miss it.
static AGAIN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Drain the spool on a worker thread.
///
/// **Never the main loop and never signal context.** A flush opens a socket and can block for
/// [`sender`]'s whole timeout; on the main thread that is a visibly frozen interface, and from a
/// signal handler it is neither async-signal-safe nor able to finish.
///
/// Returns immediately. A refused spawn is a return value rather than a panic — `task::spawn_small`
/// exists because `thread::spawn` panics on EAGAIN and killed this app once.
pub fn flush_soon() {
    use std::sync::atomic::Ordering;
    if !sender::configured() {
        // Nothing in this build to send to — see `sender`'s module doc. A report the person was
        // shown a Report ID for can never get through, so it says so instead of spinning.
        delivery::fail_unsettled();
        return;
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
    AGAIN.store(true, Ordering::Release);
    if FLUSHING.swap(true, Ordering::AcqRel) {
        return; // one at a time — see FLUSHING; the running one looks again (AGAIN)
    }
    AGAIN.store(false, Ordering::Release);
    let ok = plx_base::task::spawn_small("telemetry", move || {
        let retry = flush_now(&c, decision_revision);
        FLUSHING.store(false, Ordering::Release);
        if AGAIN.load(Ordering::Acquire) {
            flush_soon();
        }
        if let Some(seconds) = retry {
            if RETRY_SCHEDULED.swap(true, Ordering::AcqRel) {
                return;
            }
            // A retry is an actual schedule, not merely a number in a log. This worker owns no
            // spool lock and has a small stack; when it wakes it goes through FLUSHING again.
            let scheduled = plx_base::task::spawn_small("telemetry-retry", move || {
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
    let (newly_retired, retry) = process_records(
        &all,
        c,
        || consent::revision() == decision_revision,
        sender::send_one,
    );
    retired.extend(newly_retired);
    if let Some(s) = retry {
        plx_base::eventlog::log(&format!(
            "telemetry: holding {} records, ~{s}s",
            all.len() - retired.len()
        ));
    }
    if !retired.is_empty() {
        spool::commit_retiring(&retired);
        plx_base::eventlog::log(&format!(
            "telemetry: flushed {} of {} record(s)",
            retired.len(),
            all.len()
        ));
    }
    retry
}

/// Process each destination as an independent logical lane. A dead/rate-limited Sentry endpoint
/// cannot prevent a later PostHog record from being attempted, or vice versa.
///
/// Every outcome is also settled in [`delivery`] against the record's event id — a no-op for a
/// record nobody is watching — so a Report ID on screen follows what the server actually said:
/// accepted is `Delivered`, refused or retired unsent is `Failed`, and "not now" is `Held` for the
/// record it was said about AND every record behind it in that lane, which stays spooled untried.
fn process_records(
    all: &[queue::Record],
    c: &consent::Consent,
    mut still_current: impl FnMut() -> bool,
    mut send: impl FnMut(&queue::Record) -> (sender::Verdict, Option<u64>),
) -> (Vec<String>, Option<u64>) {
    let mut retired = Vec::new();
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
            // Per record, against its own category — a spool written before a withdrawal can still
            // hold records of a category that is now off.
            if !sender::allowed(r, c) {
                delivery::settle(&r.event_id, delivery::DeliveryState::Failed);
                retired.push(r.event_id.clone());
                continue;
            }
            match send(r) {
                (sender::Verdict::Done, _) => {
                    delivery::settle(&r.event_id, delivery::DeliveryState::Delivered);
                    retired.push(r.event_id.clone())
                }
                (sender::Verdict::Hopeless, _) => {
                    delivery::settle(&r.event_id, delivery::DeliveryState::Failed);
                    retired.push(r.event_id.clone())
                }
                // Stop this lane only. The failure applies to later records for the same endpoint,
                // but says nothing about the independent service in the other lane.
                (sender::Verdict::Keep, hold) => {
                    let s = hold.unwrap_or(sender::DEFAULT_HOLD_S);
                    retry = Some(retry.map_or(s, |old| old.min(s)));
                    for held in all.iter().filter(|h| h.dest == dest).skip_while(|h| h.event_id != r.event_id) {
                        delivery::settle(&held.event_id, delivery::DeliveryState::Held);
                    }
                    break;
                }
            }
        }
    }
    (retired, retry)
}

/// 16 bytes of `/dev/urandom` as lowercase hex — the ONLY way a consent identifier (the analytics
/// `install_id` or the crash-report `errors_id`) is ever produced.
///
/// Reads the device directly rather than taking a dependency: this crate has no RNG, and the one
/// property that matters is that the value is not derived from anything about this television or
/// this account. A read failure yields `None`, and [`consent::apply`] then records that channel as
/// off rather than inventing a fallback — a "random" identifier built from a clock or a MAC is
/// exactly the identifier this design refuses.
pub fn mint_id() -> Option<String> {
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
pub fn is_minted_id(s: &str) -> bool {
    s.len() == 32
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let (retired, retry) = process_records(
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
        assert_eq!(retry, Some(7));
    }

    fn watched_record(id: &str, category: queue::Category) -> queue::Record {
        assert!(delivery::watch(id, delivery::DeliveryState::Queued, delivery::tenure()));
        queue::Record { category, dest: queue::Dest::Sentry, event_id: id.into(), body: b"{}".to_vec() }
    }

    /// **(a)–(c), the one-off lane: a queued report is settled by what the flush actually heard.**
    /// Queued until a server answers; a 2xx is Delivered; a refusal or a record retired unsent is
    /// Failed; "not now" — and every record the held lane did not get to — is Held, still spooled.
    #[test]
    fn the_flush_settles_each_watched_one_off_by_what_the_server_said() {
        use delivery::DeliveryState as D;
        let _g = plx_base::testlock::serial();
        delivery::forget();
        let all = vec![
            watched_record("done", queue::Category::OneOff),
            watched_record("refused", queue::Category::OneOff),
            watched_record("held", queue::Category::OneOff),
            watched_record("behind", queue::Category::OneOff),
        ];
        assert_eq!(delivery::state("done"), Some(D::Queued), "queued is not delivered");
        let c = consent::Consent::default();
        process_records(&all, &c, || true, |r| match r.event_id.as_str() {
            "done" => (sender::Verdict::Done, None),
            "refused" => (sender::Verdict::Hopeless, None),
            _ => (sender::Verdict::Keep, Some(60)),
        });
        let states: Vec<_> = ["done", "refused", "held", "behind"].iter().map(|id| delivery::state(id)).collect();
        assert_eq!(states, vec![Some(D::Delivered), Some(D::Failed), Some(D::Held), Some(D::Held)]);
        delivery::forget();
    }

    /// **(d), the standing lane: the same verdicts, and a withdrawal that retires it unsent is a
    /// failure, not a delivery.**
    #[test]
    fn the_flush_settles_each_watched_standing_report_the_same_way() {
        use delivery::DeliveryState as D;
        let _g = plx_base::testlock::serial();
        delivery::forget();
        let on = consent::Consent {
            asked_version: consent::POLICY_VERSION,
            errors: true,
            errors_id: Some("e".repeat(32)),
            ..Default::default()
        };
        let all = vec![
            watched_record("s-done", queue::Category::Errors),
            watched_record("s-held", queue::Category::Errors),
        ];
        process_records(&all, &on, || true, |r| {
            if r.event_id == "s-done" { (sender::Verdict::Done, None) } else { (sender::Verdict::Keep, None) }
        });
        assert_eq!((delivery::state("s-done"), delivery::state("s-held")), (Some(D::Delivered), Some(D::Held)));

        let withdrawn = vec![watched_record("s-withdrawn", queue::Category::Errors)];
        process_records(&withdrawn, &consent::Consent::default(), || true, |_| panic!("sent after a withdrawal"));
        assert_eq!(delivery::state("s-withdrawn"), Some(D::Failed));
        delivery::forget();
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
    /// `app::session_worker_adapter_tests::signing_out_leaves_no_consent_and_no_identifier_for_the_next_account`
    /// grades the same thing through the real sign-out tail.
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
        let _g = plx_base::testlock::serial();
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
        assert!(persistence::load(std::slice::from_ref(&file)).any());
        assert!(consent::errors_id().is_some() && consent::allows_usage());
        forget();
        let after = consent::current().expect("a default decision is published, not none");
        assert!(!after.any() && !after.answered());
        assert!(after.install_id.is_none() && after.errors_id.is_none());
        assert!(consent::errors_id().is_none() && !consent::allows_errors());
        let reopened = persistence::load(std::slice::from_ref(&file));
        assert!(!reopened.answered() && reopened.install_id.is_none() && reopened.errors_id.is_none());
        assert!(!file.exists(), "the decision file survived");
    }

    /// An unreadable or corrupt file is the DEFAULT decision, never a partial one — a file we
    /// cannot understand is not consent.
    #[test]
    fn an_unparsable_file_is_not_consent() {
        let c: Consent = serde_json::from_slice(b"{ not json").unwrap_or_default();
        assert!(!c.any() && !c.answered());
    }

    /// The in-place upgrade regression: a 0.6.6 install keeps its decision in the canonical record
    /// (no legacy file remains), and a 0.6.5 file carries accepted and declined scopes. The boot
    /// read must find both, and the next answer must not narrow them or downgrade the policy.
    #[test]
    fn an_upgraded_06_decision_is_kept_and_the_next_answer_does_not_narrow_it() {
        let _g = plx_base::testlock::serial();
        let dir = std::env::temp_dir().join(format!("plxnative-consent-upgrade-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let saved = consent::current();
        let legacy = dir.join("telemetry.json");
        redirect_for_test(Some(legacy.clone()));
        spool::set_test_path(Some(dir.join("spool.jsonl")));
        plx_base::paths::redirect_persistent_state_root_for_test(Some(dir.clone()));

        std::fs::write(
            dir.join("consent.json"),
            include_str!("../../../../tests/fixtures/persistence/generated/v0.6.6-json-store/consent.json"),
        )
        .unwrap();
        let from_066 = serde_json::to_value(load_from(std::slice::from_ref(&legacy))).unwrap();
        assert_eq!(
            (&from_066["errors"], &from_066["errors_id"], &from_066["errors_declined_scope"]),
            (&serde_json::json!(true), &serde_json::json!("0123456789abcdef0123456789abcdef"), &serde_json::json!(6)),
            "a 0.6.6 decision reverted to unanswered on upgrade"
        );

        std::fs::remove_file(dir.join("consent.json")).unwrap();
        std::fs::write(
            &legacy,
            include_str!("../../../../tests/fixtures/persistence/generated/v0.6.5-errors-yes-declined-extension.consent.json"),
        )
        .unwrap();
        let prev = load_from(std::slice::from_ref(&legacy));
        record(consent::apply(&prev, true, false, || None));
        let after = serde_json::to_value(load_from(std::slice::from_ref(&legacy))).unwrap();
        assert_eq!(after["asked_version"], 6, "policy version downgraded");
        assert_eq!(
            (&after["errors_scope"], &after["errors_declined_scope"]),
            (&serde_json::json!(4), &serde_json::json!(6)),
            "the accepted and declined scopes were dropped"
        );

        spool::set_test_path(None);
        redirect_for_test(None);
        plx_base::paths::redirect_persistent_state_root_for_test(None);
        if let Some(c) = saved {
            consent::install(c);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_symlink_cannot_supply_telemetry_consent() {
        use std::os::unix::fs::symlink;
        let _g = plx_base::testlock::serial();
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
