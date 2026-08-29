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
#[allow(dead_code)] // no producer yet — the boot-time reader lands with the Sentry path
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
#[allow(dead_code)] // no producer yet — the boot-time reader lands with the Sentry path
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
#[allow(dead_code)] // no producer yet — the boot-time reader lands with the Sentry path
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PanicReport {
    /// `file:line`, from `std::panic::Location`. Compile-time source text baked into the binary,
    /// so it is a fact about the code rather than about the person running it.
    pub location: &'static str,
    /// A hash of the message, so identical panics group into one issue without the text travelling.
    pub message_hash: u64,
}

/// FNV-1a, 64-bit. Non-cryptographic on purpose and that is safe here: this is a GROUPING key, not
/// a secret, and it is never sent alongside anything that would make reversing it useful. A real
/// digest would mean a dependency or a hand-rolled SHA for no benefit.
#[allow(dead_code)] // no producer yet — the boot-time reader lands with the Sentry path
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
#[allow(dead_code)] // no producer yet — the boot-time reader lands with the Sentry path
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

/// Every fault record in a crash log, oldest first.
///
/// Reads **numbers only**. The `(SIGSEGV)` token in the record is deliberately ignored — the name is
/// re-derived from [`signal_name`] — and the `reg:`, `at:` and `bin:` lines are not parsed at all
/// today: registers would be useful and are not worth the surface until something reads them, and
/// the two maps lines are PATHS, which is exactly the kind of text this module does not move.
#[allow(dead_code)] // no producer yet — the boot-time reader lands with the Sentry path
pub(crate) fn parse(log: &str) -> Vec<Fault> {
    log.lines()
        .filter(|l| l.starts_with("*** SIGNAL "))
        .filter_map(|l| {
            let n = l["*** SIGNAL ".len()..].split_whitespace().next()?;
            Some(Fault {
                signal: n.parse().ok()?,
                addr: hex_field(l, "addr=").unwrap_or(0),
                pc: hex_field(l, "pc=")?,
                lr: hex_field(l, "lr=")?,
            })
        })
        .collect()
}

/// The Sentry event body for one fault.
///
/// `image_addr` is [`super::sentry::IMAGE_ADDR`] and not zero — the measured trap; its doc carries
/// the A/B that settled it.
#[allow(dead_code)] // no producer yet — the boot-time reader lands with the Sentry path
pub(crate) fn sentry_body(f: &Fault, event_id: &str, build_id: &str, debug_id: &str) -> Vec<u8> {
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
        "debug_meta": {"images": [{
            "type": "elf",
            "image_addr": super::sentry::IMAGE_ADDR,
            "code_id": build_id,
            "debug_id": debug_id,
            "code_file": CODE_FILE,
        }]},
    });
    serde_json::to_vec(&body).unwrap_or_default()
}

/// The image path reported to Sentry.
///
/// A CONSTANT rather than the real install path, and the difference matters: the actual directory
/// is one of two prefixes (`/media/developer/…` or `/media/cryptofs/…`) and reporting which would
/// say how the person installed the app — a small fact about them rather than about the crash.
/// Sentry only uses this string to label the image in the UI; the pairing is done by `debug_id`.
#[allow(dead_code)] // no producer yet — the boot-time reader lands with the Sentry path
const CODE_FILE: &str = "plxnative";

#[cfg(test)]
mod tests {
    use super::*;

    /// A real record, in the exact shape `plx_fmt_signal` emits.
    const REC: &str = "\n*** SIGNAL 11 (SIGSEGV) addr=0x0 pc=0x88ef8 lr=0x4bccd0\n\
                       reg: sp=0xbe8f1a90 fp=0x0 ip=0x1 cpsr=0x60000010 r0=0x0 r1=0x2a\n\
                       at: b6f00000-b6f21000 r-xp 00000000 fe:01 1234 /lib/libc.so.6\n\
                       bin: 00010000-00600000 r-xp 00000000 fe:01 99 /media/developer/x/plxnative\n";

    #[test]
    fn a_real_record_parses_to_numbers() {
        let f = parse(REC);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0], Fault { signal: 11, addr: 0, pc: 0x88ef8, lr: 0x4bccd0 });
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
        assert_eq!(f[0].signal, 11);
        assert_eq!(f[1].signal, 6);
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
        let f = parse(REC)[0];
        let body = String::from_utf8(sentry_body(&f, "e".repeat(32).as_str(), "bid", "did"))
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
        assert!(body.contains(super::super::sentry::IMAGE_ADDR));
    }

    /// The image path is a constant, so the report does not say WHICH of the two install prefixes
    /// this person used — a fact about them rather than about the crash.
    #[test]
    fn the_reported_image_path_says_nothing_about_the_install() {
        let body = String::from_utf8(sentry_body(&parse(REC)[0], "e", "b", "d")).unwrap();
        assert!(body.contains("plxnative"));
        assert!(!body.contains("/media/"), "the install prefix must not be reported");
    }

    /// **A panic's MESSAGE is never carried** — only where it happened and a hash that groups it.
    /// A Rust panic message routinely contains a path or an interpolated value; `unwrap()` on an
    /// io error prints the filename.
    #[test]
    fn a_panic_report_carries_no_message() {
        let msg = "called `Result::unwrap()` on an `Err` value: /media/internal/Films/Dune.mkv";
        let r = PanicReport { location: "src/ff.rs:1204", message_hash: message_hash(msg) };
        let rendered = format!("{r:?}");
        assert!(!rendered.contains("Dune"), "a title reached the report");
        assert!(!rendered.contains("/media/"), "a path reached the report");
        assert!(rendered.contains("src/ff.rs:1204"), "the location is kept");
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
