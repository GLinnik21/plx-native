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
//! destroys what it withdrew, that the event path fails closed, that a record queued while a flush
//! was on the network is not erased by that flush's commit — and a test behind a feature the
//! default gate does not build is a test that never runs.
pub(crate) mod consent;
pub(crate) mod crashreport;
pub(crate) mod native;
pub(crate) mod playback;
pub(crate) mod posthog;
pub(crate) mod queue;
pub(crate) mod sender;
pub(crate) mod sentry;
pub(crate) mod spool;

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
    load_from(&crate::paths::telemetry_candidates())
}

fn load_from(candidates: &[std::path::PathBuf]) -> Consent {
    candidates
        .iter()
        .filter_map(|p| crate::plex::session::read_owned_regular(p))
        .find_map(|b| serde_json::from_slice::<Consent>(&b).ok())
        .unwrap_or_default()
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
    let stored = crate::paths::telemetry_candidates()
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
    }
    consent::install(c.clone());
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

/// Withdraw every in-memory telemetry permission and purge queued/native reports without writing a
/// replacement consent file. Used by full local-data erasure, where recreating even a default
/// settings file would make the operation's name false.
pub(crate) fn forget_local() {
    let c = Consent::default();
    consent::install(c.clone());
    spool::purge_withdrawn(&c);
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

/// Queue one record for later. Main-thread-safe, and it does NOT send.
pub(crate) fn enqueue(r: queue::Record) {
    spool::append(&r);
}

/// Drain the spool on a worker thread.
///
/// **Never the main loop and never signal context.** A flush opens a socket and can block for
/// [`sender`]'s whole timeout; on the main thread that is a visibly frozen interface, and from a
/// signal handler it is neither async-signal-safe nor able to finish.
///
/// Returns immediately. A refused spawn is a return value rather than a panic — `task::spawn_small`
/// exists because `thread::spawn` panics on EAGAIN and killed this app once.
pub(crate) fn flush_soon() {
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
    let (newly_retired, retry) = process_records(
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
            // Never carry an old consent snapshot through a withdrawal or a quick off→on cycle.
            // A request that already passed this check may finish because the socket API has no
            // cancellation; PRIVACY.md states that narrow in-flight boundary explicitly.
            if !still_current() {
                break 'destinations;
            }
            // Per record, against its own category — a spool written before a withdrawal can still
            // hold records of a category that is now off.
            if !sender::allowed(r, c) {
                retired.push(r.event_id.clone());
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

    /// **Delete all local data leaves neither identifier in the snapshot.** `app.rs` removes the
    /// consent file itself; this is the in-memory half, and it is the half a producer on the
    /// render thread reads, so a report queued after the deletion must find nothing to attach.
    #[test]
    fn forgetting_local_data_clears_both_identifiers_from_the_snapshot() {
        let _g = crate::testlock::serial();
        let saved = consent::current();
        consent::install(consent::apply(&Consent::default(), true, true, || {
            Some("f".repeat(32))
        }));
        assert!(consent::errors_id().is_some() && consent::allows_usage());
        forget_local();
        let after = consent::current().expect("a default decision is published, not none");
        assert!(!after.any() && !after.answered());
        assert!(after.install_id.is_none() && after.errors_id.is_none());
        assert!(consent::errors_id().is_none() && !consent::allows_errors());
        if let Some(c) = saved {
            consent::install(c);
        }
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
