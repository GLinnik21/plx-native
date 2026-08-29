//! **The crash channel, and the guarantee that it carries no text.**
//!
//! Reads the append-only crash log the C tracer writes (`src/crashtrace.c`), turns each fault
//! record into a Sentry event, and reports it on the next launch — because the process that wrote
//! it no longer exists, which is the whole reason the log is on disk and not in memory.
//!
//! # NOTHING FROM THE LOG'S TEXT REACHES THE WIRE
//!
//! This is a decision, taken deliberately after the alternative was considered and rejected: the
//! app does not send its logs, and the crash channel does not carry breadcrumbs, messages, or any
//! string lifted out of a file.
//!
//! It is enforced structurally rather than by care. [`parse`] reads **numbers only** — the signal
//! number, `addr`, `pc`, `lr`, and the registers — and the signal's NAME is then derived from
//! [`signal_name`], a fixed table in this file. So even the one human-readable token in the record
//! (`SIGSEGV`) reaches Sentry as a `&'static str` this crate owns, not as bytes that happened to be
//! in a file. A tracer that one day wrote something unexpected there could not smuggle it out.
//!
//! The same rule kills the obvious shortcut of shipping the record verbatim as the exception
//! `value`. That would be one line of code, would look identical in the Sentry UI, and would make
//! the whole guarantee a matter of what the tracer happens to write.
//!
//! # The panic message is the sharp edge, and it is not sent
//!
//! The plan this was built to flagged an inversion worth restating: an allowlist that guards
//! breadcrumbs while the *exception message* is free-text panic payload protects the cheap channel
//! and leaves the expensive one open. A Rust panic message routinely carries a path, a URL, or an
//! interpolated value — `unwrap()` on a `Result<_, io::Error>` prints the filename.
//!
//! So [`PanicReport`] carries the **location** (`file:line`, a `&'static str` baked into the binary
//! at compile time, and therefore source text rather than anybody's data) and a **hash** of the
//! message. The hash still groups identical panics into one Sentry issue, which is most of what the
//! message was for; the text itself never leaves. `docs/…` aside, the practical consequence is that
//! a panic tells you WHERE and HOW OFTEN, and you read WHAT in the log on the television.
//!
//! # Sending twice is worse than not sending
//!
//! `plxnative-crash.log` is append-only and survives relaunch **by design** — CLAUDE.md calls it the
//! thing to read after a crash-and-restart, and `tools/crash-report.sh` parses it. So this module
//! may not truncate it. Instead it records how many bytes it has already reported and skips them,
//! which means a human and this module can both read the file without either disturbing the other.

/// The one place a signal number becomes a name. See the module doc: this exists so the name on the
/// wire comes from a table this crate owns rather than from bytes in a file.
pub(crate) fn signal_name(sig: u32) -> &'static str {
    match sig {
        4 => "SIGILL",
        6 => "SIGABRT",
        7 => "SIGBUS",
        8 => "SIGFPE",
        11 => "SIGSEGV",
        5 => "SIGTRAP",
        _ => "SIGNAL",
    }
}

/// One fault, as numbers. Every field is an integer; there is no `String` in this type, which is
/// what makes "the crash channel carries no text" a property of the type rather than a promise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Fault {
    pub signal: u32,
    /// `si_addr` — the address the fault was ABOUT (0 for a null dereference).
    pub addr: u64,
    /// The faulting instruction.
    pub pc: u64,
    /// The link register — the caller, and in practice the more useful of the two, since `pc` is
    /// often inside a library whose symbols we do not have.
    pub lr: u64,
}

/// A Rust panic, reduced to what may be sent. **No message**: see the module doc.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PanicReport {
    /// `file:line`, from `std::panic::Location` by way of the crash log. Compile-time source text
    /// baked into the binary, so it is a fact about the code rather than about the person running
    /// it — but it arrives here as bytes read from a FILE, so it is validated by
    /// [`looks_like_a_source_location`] rather than trusted. That check is the whole difference
    /// between "our own source path" and "whatever was on that line".
    pub location: String,
    /// A hash of the message, so identical panics group into one issue without the text travelling.
    pub message_hash: u64,
}

/// Whether a string is plausibly one of OUR source locations, and therefore safe to send.
///
/// The location is the one field this module lifts out of the log as text, so it gets the treatment
/// [`parse`]'s numbers get for free: it must end in `:<digits>`, name a `.rs` file, carry no
/// whitespace and be short. A tracer, a corrupted log or a future format change cannot then smuggle
/// a title, a URL or a path out through the one gap in the no-text rule.
pub(crate) fn looks_like_a_source_location(s: &str) -> bool {
    const MAX: usize = 120;
    let Some((file, line)) = s.rsplit_once(':') else { return false };
    !s.is_empty()
        && s.len() <= MAX
        && !s.chars().any(char::is_whitespace)
        && file.ends_with(".rs")
        && !line.is_empty()
        && line.chars().all(|c| c.is_ascii_digit())
}

/// FNV-1a, 64-bit. Non-cryptographic on purpose and that is safe here: this is a GROUPING key, not
/// a secret, and it is never sent alongside anything that would make reversing it useful. A real
/// digest would mean a dependency or a hand-rolled SHA for no benefit.
pub(crate) fn message_hash(msg: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in msg.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x1000_0000_01b3);
    }
    h
}

/// Pull `0x…` after a `key=` marker, as a number. Returns `None` rather than 0 for absent, so a
/// missing field is distinguishable from a genuine zero — `addr=0x0` is what a null dereference
/// looks like and is the most common real fault, so conflating the two would misreport the
/// commonest crash there is.
fn hex_field(line: &str, key: &str) -> Option<u64> {
    let at = line.find(key)? + key.len();
    let rest = &line[at..];
    let digits = rest.strip_prefix("0x").unwrap_or(rest);
    let end = digits.find(|c: char| !c.is_ascii_hexdigit()).unwrap_or(digits.len());
    if end == 0 {
        return None;
    }
    u64::from_str_radix(&digits[..end], 16).ok()
}

/// One thing worth reporting, in the order the crash log recorded it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Report {
    Fault(Fault),
    Panic(PanicReport),
}

/// SIGABRT. Named because the coalescing rule below turns on it and a bare `6` would not say why.
const SIGABRT: u32 = 6;

/// Every report in a crash log, oldest first, **with a panic and the abort it caused counted once**.
///
/// Faults read **numbers only**. The `(SIGSEGV)` token in the record is deliberately ignored — the
/// name is re-derived from [`signal_name`] — and the `reg:`, `at:` and `bin:` lines are not parsed
/// at all: registers would be useful and are not worth the surface until something reads them, and
/// the two maps lines are PATHS, which is exactly the kind of text this module does not move.
///
/// # A panic that crosses FFI is ONE fault, and it writes TWO records
///
/// Unwinding out of an `extern "C"` frame aborts the process, so `libav` calling `ff::read_cb` on a
/// panicking path produces the panic line AND a `*** SIGNAL 6` immediately after it, from the same
/// fault. Sent naively that is two Sentry events for one crash, which does not merely add noise: it
/// doubles the crash-free rate's numerator, and that number is the headline this whole channel
/// exists to produce.
///
/// The rule is positional and deliberately narrow — a SIGABRT whose immediately preceding record is
/// a panic is that panic's abort — because the two are written microseconds apart by the same
/// thread with nothing able to interleave. A SIGABRT arriving any other way is a real, separate
/// abort (a failed assertion in a C library, a double free) and is reported. The panic is the one
/// kept, being the report that says WHERE.
pub(crate) fn parse(log: &str) -> Vec<Report> {
    let mut out: Vec<Report> = Vec::new();
    for l in log.lines() {
        if let Some(rest) = l.strip_prefix("*** SIGNAL ") {
            let Some(f) = parse_fault(rest, l) else { continue };
            if f.signal == SIGABRT && matches!(out.last(), Some(Report::Panic(_))) {
                continue; // the panic above it is the report — see the doc
            }
            out.push(Report::Fault(f));
        } else if l.starts_with("*** RUST PANIC ") {
            if let Some(p) = parse_panic(l) {
                out.push(Report::Panic(p));
            }
        }
    }
    out
}

fn parse_fault(rest: &str, whole: &str) -> Option<Fault> {
    Some(Fault {
        signal: rest.split_whitespace().next()?.parse().ok()?,
        addr: hex_field(whole, "addr=").unwrap_or(0),
        pc: hex_field(whole, "pc=")?,
        lr: hex_field(whole, "lr=")?,
    })
}

/// `*** RUST PANIC [thread] at <file>:<line>: <message>` — the line `app.rs`'s hook writes.
///
/// The message is **hashed and discarded on this line**, never carried in a field somebody could
/// later decide to send. The thread name is not read at all: it is a `&'static str` in our own
/// source, but reading it would put a second free-text field on this path for no diagnostic gain.
fn parse_panic(l: &str) -> Option<PanicReport> {
    let after = l.find(" at ")? + " at ".len();
    let rel = l[after..].find(": ")?;
    let location = &l[after..after + rel];
    if !looks_like_a_source_location(location) {
        return None;
    }
    Some(PanicReport {
        location: location.to_string(),
        message_hash: message_hash(&l[after + rel + 2..]),
    })
}

/// The Sentry event body for one fault.
///
/// `image_addr` comes from [`super::sentry::image_addr`] — the running binary's own lowest
/// `PT_LOAD`, not zero. Its doc carries the measured A/B that settled why zero is the trap.
pub(crate) fn sentry_body(f: &Fault, event_id: &str, build_id: &str, debug_id: Option<&str>) -> Vec<u8> {
    let name = signal_name(f.signal);
    let body = serde_json::json!({
        "event_id": event_id,
        "platform": "native",
        "level": "fatal",
        // **The value is built from the NAME and the ADDRESS, both of which this crate owns.** Not
        // the record's own line, which would be one line of code and would make the no-text
        // guarantee depend on what the C tracer happens to write.
        "exception": {"values": [{
            "type": name,
            "value": format!("{name} at 0x{:x}", f.addr),
            "mechanism": {"type": "signalhandler", "handled": false,
                          "meta": {"signal": {"number": f.signal, "name": name}}},
            "stacktrace": {"frames": [
                // Oldest first, which is the order Sentry renders bottom-up: lr is the caller, so
                // it goes first and pc — the faulting instruction — ends up on top.
                {"instruction_addr": format!("0x{:x}", f.lr), "platform": "native"},
                {"instruction_addr": format!("0x{:x}", f.pc), "platform": "native"},
            ]},
        }]},
    });
    // **No debug image rather than a broken one.** An entry whose `debug_id` matches no uploaded
    // object yields `missing_symbol` and NO error — see `sentry::IMAGE_ADDR`'s measured table — so a
    // build whose id could not be read is better off saying nothing than asserting a pairing that
    // does not exist. The addresses are still in the frames, and `code_id` still names the build.
    let mut body = body;
    if let (Some(did), false) = (debug_id, build_id.is_empty()) {
        body["debug_meta"] = serde_json::json!({"images": [{
            "type": "elf",
            "image_addr": super::sentry::image_addr(),
            "code_id": build_id,
            "debug_id": did,
            "code_file": CODE_FILE,
        }]});
    }
    serde_json::to_vec(&body).unwrap_or_default()
}

/// The Sentry event body for one Rust panic.
///
/// No stacktrace: the panic line carries a source location and no addresses, and inventing frames
/// from nothing is how a symbolicator is handed garbage. **`fingerprint` is therefore explicit** —
/// with no exception addresses to group by, Sentry would fall back on the `value` string, which
/// here is a hash and would put every distinct panic message in its own issue while telling you
/// nothing about which. Grouping on `(location, hash)` says "this panic, at this line", which is
/// what the message would have been used for.
pub(crate) fn panic_sentry_body(p: &PanicReport, event_id: &str) -> Vec<u8> {
    let body = serde_json::json!({
        "event_id": event_id,
        "platform": "native",
        "level": "fatal",
        "exception": {"values": [{
            "type": "panic",
            // The hash, NOT the message. See the module doc: this is the field a Rust panic would
            // otherwise fill with an interpolated path, URL or value.
            "value": format!("Rust panic at {} (msg {:016x})", p.location, p.message_hash),
            "mechanism": {"type": "panic", "handled": false},
        }]},
        "fingerprint": ["rust-panic", p.location.clone(), format!("{:016x}", p.message_hash)],
        "culprit": p.location.clone(),
    });
    serde_json::to_vec(&body).unwrap_or_default()
}

/// **A Sentry `event_id` derived from the report itself, so a retry is idempotent.**
///
/// The alternative — mint a random id and remember it — puts the idempotency key in the same file
/// whose loss is the reason a retry happens at all. Deriving it means the same fault produces the
/// same id however many times it is re-read: Sentry accepts the first and discards the rest, so a
/// crash that is reported twice because a flush was interrupted stays ONE issue with one count,
/// rather than inflating the number this channel exists to measure.
///
/// The build id is in the key because the same source line in two builds is two different reports;
/// so is the fault's own position in the log, or a television that faulted identically twice would
/// report once. 128 bits from two FNV passes with different seeds — a GROUPING key, not a secret,
/// and the collision that matters is between two records in one log.
pub(crate) fn event_id_for(build_id: &str, seq: usize, r: &Report) -> String {
    let key = match r {
        Report::Fault(f) => {
            format!("{build_id}|{seq}|sig{}|{:x}|{:x}|{:x}", f.signal, f.addr, f.pc, f.lr)
        }
        Report::Panic(p) => format!("{build_id}|{seq}|panic|{}|{:x}", p.location, p.message_hash),
    };
    let a = message_hash(&key);
    let b = message_hash(&format!("{key}|2"));
    format!("{a:016x}{b:016x}")
}

/// How much of the append-only crash log has already been reported.
#[derive(serde::Serialize, serde::Deserialize, Default)]
struct Mark {
    reported_bytes: u64,
}

/// **Report every fault the last run left behind, once.**
///
/// Called at boot, after consent is published and before anything else can crash — the process that
/// wrote these records no longer exists, which is the whole reason the log is on disk.
///
/// Three things about the bookkeeping are load-bearing.
///
/// **The mark advances only after the records are durably queued**, never before the enqueue, or a
/// spool write that fails leaves the report both unsent and permanently skipped.
///
/// **A log SHORTER than the mark means the file was replaced** — a factory reset, a reinstall, or
/// somebody clearing `/tmp` — so the mark is reset to zero rather than used to skip records that
/// are not the ones it was counting. Keeping it would silently drop exactly the crashes from a
/// freshly reinstalled app, which is when they matter most.
///
/// **No debug image without a build id.** An image entry carrying an empty or wrong `debug_id` does
/// not degrade to an unsymbolicated frame with a warning; it produces `missing_symbol` and no error
/// at all, which is indistinguishable from never having uploaded symbols. Sending no image at least
/// says so.
pub(crate) fn report_pending() {
    if !super::consent::allows_errors() {
        return; // no consent for this category — nothing is read and nothing is queued
    }
    let path = crate::paths::in_runtime_dir("plxnative-crash.log");
    let Ok(bytes) = std::fs::read(&path) else { return };

    let from = resume_from(bytes.len() as u64, read_mark().reported_bytes) as usize;
    if from >= bytes.len() {
        return;
    }
    // Lossy on purpose: this file is written by a signal handler and by the panic hook, and one
    // torn multi-byte sequence in it must not cost the whole report.
    let fresh = String::from_utf8_lossy(&bytes[from..]);
    let reports = parse(&fresh);
    if reports.is_empty() {
        // Still advance: the bytes were read and held nothing to report, and leaving the mark
        // behind would re-scan them on every boot forever.
        write_mark(bytes.len() as u64);
        return;
    }

    let bid = super::sentry::build_id();
    let did = super::sentry::debug_id(bid);
    let mut queued = 0usize;
    for (i, r) in reports.iter().enumerate() {
        let event_id = event_id_for(bid, from + i, r);
        let body = match r {
            Report::Fault(f) => sentry_body(f, &event_id, bid, did.as_deref()),
            Report::Panic(p) => panic_sentry_body(p, &event_id),
        };
        let ok = super::spool::append(&super::queue::Record {
            category: super::queue::Category::Errors,
            dest: super::queue::Dest::Sentry,
            event_id,
            body,
        });
        if !ok {
            break; // do NOT advance past a record that did not reach the disk
        }
        queued += 1;
    }
    crate::log(&format!(
        "telemetry: crash log had {} report(s), queued {queued}, symbols={}",
        reports.len(),
        if did.is_some() { "yes" } else { "no" }
    ));
    if queued == reports.len() {
        write_mark(bytes.len() as u64);
    }
}

/// Where in the crash log to start reading, given its size and the watermark.
///
/// Pure, because the interesting case cannot be produced on demand: **a log SHORTER than the mark
/// means the file was replaced** — a reinstall, a factory reset, somebody clearing `/tmp` — and the
/// mark is then counting bytes that no longer exist. Using it would skip the first N bytes of a
/// brand-new log, silently dropping exactly the crashes of a freshly reinstalled app, which is when
/// they matter most. It resets instead, and re-reporting is bounded by the deterministic
/// [`event_id_for`], which Sentry dedupes.
fn resume_from(log_len: u64, mark: u64) -> u64 {
    if log_len < mark {
        crate::log("telemetry: crash log is shorter than the watermark — reading it from the start");
        return 0;
    }
    mark
}

fn read_mark() -> Mark {
    crate::paths::telemetry_crashmark_candidates()
        .iter()
        .filter_map(|p| std::fs::read(p).ok())
        .find_map(|b| serde_json::from_slice::<Mark>(&b).ok())
        .unwrap_or_default()
}

fn write_mark(reported_bytes: u64) {
    let Ok(json) = serde_json::to_vec(&Mark { reported_bytes }) else { return };
    let stored = crate::paths::telemetry_crashmark_candidates()
        .iter()
        .any(|p| crate::plex::session::write_atomic(p, &json));
    if !stored {
        // Loud, because the consequence is re-reporting the same crash on every boot until it
        // succeeds — bounded by the deterministic `event_id`, which Sentry dedupes, but still a
        // request per launch that says nothing new.
        crate::log("telemetry: could not persist the crash watermark to ANY candidate path");
    }
}

/// The image path reported to Sentry.
///
/// A CONSTANT rather than the real install path, and the difference matters: the actual directory
/// is one of two prefixes (`/media/developer/…` or `/media/cryptofs/…`) and reporting which would
/// say how the person installed the app — a small fact about them rather than about the crash.
/// Sentry only uses this string to label the image in the UI; the pairing is done by `debug_id`.
const CODE_FILE: &str = "plxnative";

#[cfg(test)]
mod tests {
    use super::*;

    /// A real record, in the exact shape `plx_fmt_signal` emits.
    const REC: &str = "\n*** SIGNAL 11 (SIGSEGV) addr=0x0 pc=0x88ef8 lr=0x4bccd0\n\
                       reg: sp=0xbe8f1a90 fp=0x0 ip=0x1 cpsr=0x60000010 r0=0x0 r1=0x2a\n\
                       at: b6f00000-b6f21000 r-xp 00000000 fe:01 1234 /lib/libc.so.6\n\
                       bin: 00010000-00600000 r-xp 00000000 fe:01 99 /media/developer/x/plxnative\n";

    /// The one fault a log holds, for the tests that are about a fault rather than about ordering.
    fn only_fault(log: &str) -> Fault {
        match parse(log).into_iter().next().expect("a report") {
            Report::Fault(f) => f,
            other => panic!("expected a fault, got {other:?}"),
        }
    }

    #[test]
    fn a_real_record_parses_to_numbers() {
        let f = parse(REC);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0], Report::Fault(Fault { signal: 11, addr: 0, pc: 0x88ef8, lr: 0x4bccd0 }));
    }

    /// **`addr=0x0` is a real value, not a missing field.** A null dereference is the commonest
    /// fault there is, so conflating "absent" with "zero" would misreport the ordinary case.
    #[test]
    fn a_null_fault_address_is_a_value_not_an_absence() {
        assert_eq!(hex_field("addr=0x0 pc=0x1", "addr="), Some(0));
        assert_eq!(hex_field("pc=0x1", "addr="), None);
    }

    /// Several crashes accumulate in an append-only log; all of them parse, oldest first.
    #[test]
    fn every_record_in_an_append_only_log_is_found() {
        let two = format!("{REC}some unrelated line\n{}", REC.replace("SIGNAL 11", "SIGNAL 6"));
        let f = parse(&two);
        assert_eq!(f.len(), 2);
        // A SIGABRT is coalesced only when a PANIC precedes it; an unrelated line does not count,
        // and two independent faults stay two reports.
        assert!(matches!(f[0], Report::Fault(Fault { signal: 11, .. })));
        assert!(matches!(f[1], Report::Fault(Fault { signal: 6, .. })));
    }

    /// A truncated or garbled record is skipped rather than parsed into a wrong one. The log is
    /// written from a signal handler, so a record cut off by a power loss is an ordinary case.
    #[test]
    fn a_truncated_record_is_skipped_not_guessed() {
        assert!(parse("*** SIGNAL 11 (SIGSEGV) addr=0x0\n").is_empty(), "no pc/lr");
        assert!(parse("*** SIGNAL  (SIGSEGV) addr=0x0 pc=0x1 lr=0x2\n").is_empty(), "no number");
        assert!(parse("").is_empty());
        assert!(parse("nothing to see").is_empty());
    }

    /// **THE GUARANTEE.** No text from the log reaches the event body.
    ///
    /// The record fixture deliberately contains a library path, an install path and a signal name;
    /// none of them may appear in what is sent. The name that IS sent comes from `signal_name`, a
    /// table this crate owns — which is why the assertion below can require `SIGSEGV` to be present
    /// while requiring the strings around it to be absent.
    #[test]
    fn no_text_from_the_crash_log_reaches_the_wire() {
        let f = only_fault(REC);
        let body = String::from_utf8(sentry_body(&f, "e".repeat(32).as_str(), "bid", Some("did")))
            .expect("utf-8");
        for leaked in [
            "/lib/libc.so.6",
            "/media/developer",
            "r-xp",
            "cpsr",
            "sp=",
            "b6f00000",
            "reg:",
            "at:",
            "bin:",
        ] {
            assert!(!body.contains(leaked), "the crash log's text reached the wire: {leaked}");
        }
        // …and the fault itself is still fully described, by numbers.
        assert!(body.contains("SIGSEGV"), "from signal_name, not from the file");
        assert!(body.contains("0x88ef8") && body.contains("0x4bccd0"));
        assert!(body.contains(super::super::sentry::image_addr()));
    }

    /// The image path is a constant, so the report does not say WHICH of the two install prefixes
    /// this person used — a fact about them rather than about the crash.
    #[test]
    fn the_reported_image_path_says_nothing_about_the_install() {
        let body = String::from_utf8(sentry_body(&only_fault(REC), "e", "b", Some("d"))).unwrap();
        assert!(body.contains("plxnative"));
        assert!(!body.contains("/media/"), "the install prefix must not be reported");
    }

    /// **A panic's MESSAGE is never carried** — only where it happened and a hash that groups it.
    /// A Rust panic message routinely contains a path or an interpolated value; `unwrap()` on an
    /// io error prints the filename.
    #[test]
    fn a_panic_report_carries_no_message() {
        let msg = "called `Result::unwrap()` on an `Err` value: /media/internal/Films/Dune.mkv";
        let r = PanicReport { location: "src/ff.rs:1204".into(), message_hash: message_hash(msg) };
        let rendered = format!("{r:?}");
        assert!(!rendered.contains("Dune"), "a title reached the report");
        assert!(!rendered.contains("/media/"), "a path reached the report");
        assert!(rendered.contains("src/ff.rs:1204"), "the location is kept");
    }

    const PANIC_LINE: &str =
        "*** RUST PANIC [demux] at src/ff.rs:1204: called `Result::unwrap()` on an `Err` value: \
         /media/internal/Films/Dune.mkv";

    /// **A panic that crossed FFI is ONE crash and it writes TWO records.** Unwinding out of an
    /// `extern "C"` frame aborts, so `libav` calling `ff::read_cb` on a panicking path leaves the
    /// panic line and a `*** SIGNAL 6` from the same fault, microseconds apart.
    ///
    /// Sent as two, that does not merely add noise — it doubles the numerator of the crash-free
    /// rate, which is the headline number this channel exists to produce. The panic is the one
    /// kept, being the report that says WHERE.
    #[test]
    fn a_panic_and_the_abort_it_caused_are_one_report() {
        let abrt = REC.replace("SIGNAL 11", "SIGNAL 6");
        let log = format!("{PANIC_LINE}\n{abrt}");
        let r = parse(&log);
        assert_eq!(r.len(), 1, "a panic and its abort were reported as {} events", r.len());
        assert!(matches!(r[0], Report::Panic(_)));
    }

    /// …and the rule is exactly that narrow. A SIGABRT arriving any other way is a real, separate
    /// abort — a failed assertion inside a C library, a double free — and losing those would be
    /// paying for the fix above with a whole class of crash.
    #[test]
    fn an_abort_no_panic_preceded_is_still_reported() {
        let abrt = REC.replace("SIGNAL 11", "SIGNAL 6");
        assert_eq!(parse(&abrt).len(), 1);
        // Nor does a panic swallow an abort that some OTHER fault separates it from.
        let log = format!("{PANIC_LINE}\n{REC}{abrt}");
        assert_eq!(parse(&log).len(), 3, "only the ADJACENT abort coalesces");
    }

    /// **The same fault read twice produces the same `event_id`**, so a retry is idempotent —
    /// Sentry accepts the first and discards the rest. The alternative is minting a random id and
    /// remembering it, which puts the idempotency key in the same file whose loss is the reason a
    /// retry happens at all.
    #[test]
    fn the_event_id_is_derived_and_therefore_stable_across_a_retry() {
        let r = &parse(REC)[0];
        let a = event_id_for("abc123", 0, r);
        assert_eq!(a, event_id_for("abc123", 0, r), "a re-read produced a different id");
        assert_eq!(a.len(), 32, "Sentry wants 32 hex characters");
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        // …and the things that make two reports DIFFERENT reports all move it.
        assert_ne!(a, event_id_for("def456", 0, r), "another build is another report");
        assert_ne!(a, event_id_for("abc123", 1, r), "the same fault twice is two reports");
        let other = &parse(&REC.replace("pc=0x88ef8", "pc=0x99000"))[0];
        assert_ne!(a, event_id_for("abc123", 0, other));
    }

    /// **The location is the ONE field lifted out of the log as text, so it is validated rather
    /// than trusted.** Everything else on the fault path is a number by construction; this is the
    /// single gap, and without a check a corrupted log or a future format change could carry an
    /// arbitrary string through it.
    #[test]
    fn only_something_shaped_like_our_own_source_path_is_accepted_as_a_location() {
        assert!(looks_like_a_source_location("src/ff.rs:1204"));
        assert!(looks_like_a_source_location("rust-modules/src/player/pump.rs:12"));
        for bad in [
            "/media/internal/Films/Dune.mkv:1", // a path that is not source
            "src/ff.rs",                        // no line
            "src/ff.rs:abc",                    // not a line number
            "Dune the Prophecy S01E01:3",       // a title with spaces
            "",
        ] {
            assert!(!looks_like_a_source_location(bad), "accepted {bad:?}");
        }
        // …and a panic line whose location fails the check yields no report at all.
        let bad = "*** RUST PANIC [x] at Some Film Title: message";
        assert!(parse(bad).is_empty());
    }

    /// The panic's message never reaches the wire even once it is a Sentry body — the earlier test
    /// grades the STRUCT, and this grades the bytes that would actually be sent.
    #[test]
    fn a_panic_body_carries_the_location_and_not_the_message() {
        let Report::Panic(p) = &parse(PANIC_LINE)[0] else { panic!("expected a panic") };
        let body = String::from_utf8(panic_sentry_body(p, &"e".repeat(32))).expect("utf-8");
        assert!(!body.contains("Dune"), "a title reached the wire");
        assert!(!body.contains("/media/"), "a path reached the wire");
        assert!(!body.contains("unwrap"), "the message reached the wire");
        assert!(body.contains("src/ff.rs:1204"), "the location is what makes it actionable");
        // Grouped on (location, hash), not on the value string — see `panic_sentry_body`.
        assert!(body.contains("fingerprint"));
    }

    /// **A log shorter than the watermark means the file was REPLACED**, so the mark is reset
    /// rather than used to skip bytes it was never counting. Keeping it would drop exactly the
    /// crashes of a freshly reinstalled app.
    #[test]
    fn a_replaced_crash_log_is_read_from_the_start() {
        assert_eq!(resume_from(4096, 1024), 1024, "the ordinary case skips what was reported");
        assert_eq!(resume_from(512, 1024), 0, "a shorter log is a new file");
        assert_eq!(resume_from(1024, 1024), 1024, "nothing new is not a reset");
    }

    /// **A build whose id could not be read sends NO debug image**, rather than one asserting a
    /// pairing that does not exist. An image with a wrong `debug_id` does not degrade to an
    /// unsymbolicated frame with a warning — it yields `missing_symbol` and no error at all, which
    /// is indistinguishable from never having uploaded symbols. See `sentry::IMAGE_ADDR`.
    #[test]
    fn no_debug_image_is_better_than_one_that_matches_nothing() {
        let f = only_fault(REC);
        let with = String::from_utf8(sentry_body(&f, "e", "bid", Some("did"))).unwrap();
        assert!(with.contains("debug_meta") && with.contains("\"debug_id\":\"did\""));

        let without = String::from_utf8(sentry_body(&f, "e", "bid", None)).unwrap();
        assert!(!without.contains("debug_meta"), "an image was claimed with no id to pair it");
        // The addresses still travel: an unsymbolicated report is worth far more than none.
        assert!(without.contains("0x88ef8") && without.contains("0x4bccd0"));

        let no_build = String::from_utf8(sentry_body(&f, "e", "", Some("did"))).unwrap();
        assert!(!no_build.contains("debug_meta"));
    }

    /// …and the hash still groups: the same message hashes alike, different ones differ. That is
    /// what makes dropping the text affordable rather than merely safe.
    #[test]
    fn the_hash_groups_identical_panics_and_separates_others() {
        assert_eq!(message_hash("index out of bounds"), message_hash("index out of bounds"));
        assert_ne!(message_hash("index out of bounds"), message_hash("divide by zero"));
        assert_ne!(message_hash(""), message_hash("x"));
    }

    /// Signal names come from the table, and an unknown number degrades to a generic name rather
    /// than to whatever was in the file.
    #[test]
    fn the_signal_name_comes_from_our_own_table() {
        assert_eq!(signal_name(11), "SIGSEGV");
        assert_eq!(signal_name(6), "SIGABRT");
        assert_eq!(signal_name(4), "SIGILL");
        assert_eq!(signal_name(7), "SIGBUS");
        assert_eq!(signal_name(999), "SIGNAL");
    }
}
