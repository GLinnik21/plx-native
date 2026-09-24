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
//! `plxnative-crash.log` is append-only and survives relaunch **by design** — `docs/agent-reference.md` calls it the
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

/// One fault as numeric machine state plus a strictly validated ELF identity. The one `String` is
/// exactly 40 lowercase hex digits parsed from our own startup marker; no path or signal text can
/// inhabit this type.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Fault {
    pub signal: u32,
    /// `si_addr` — the address the fault was ABOUT (0 for a null dereference).
    pub addr: u64,
    /// The faulting instruction.
    pub pc: u64,
    /// The link register — the caller, and in practice the more useful of the two, since `pc` is
    /// often inside a library whose symbols we do not have.
    pub lr: u64,
    /// Identity written while the crashed executable was still alive. Never inferred from the
    /// next process, which may be a newly deployed build.
    pub image: Option<ImageIdentity>,
    /// Fixed-name numeric ARM registers. No text from the log is retained.
    pub registers: Registers,
}

/// The three ELF facts needed to pair an address with its debug file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ImageIdentity {
    pub build_id: String,
    pub image_addr: u64,
    pub image_size: u64,
}

/// ARM's integer register set, parsed through a fixed key allowlist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Registers {
    pub r0: Option<u64>,
    pub r1: Option<u64>,
    pub r2: Option<u64>,
    pub r3: Option<u64>,
    pub r4: Option<u64>,
    pub r5: Option<u64>,
    pub r6: Option<u64>,
    pub r7: Option<u64>,
    pub r8: Option<u64>,
    pub r9: Option<u64>,
    pub r10: Option<u64>,
    pub fp: Option<u64>,
    pub ip: Option<u64>,
    pub sp: Option<u64>,
    pub lr: Option<u64>,
    pub pc: Option<u64>,
    pub cpsr: Option<u64>,
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
    /// The same validated ELF identity as a signal record, when a startup marker preceded it.
    pub image: Option<ImageIdentity>,
}

/// Whether a string is plausibly one of OUR source locations, and therefore safe to send.
///
/// The location is the one field this module lifts out of the log as text, so it gets the treatment
/// [`parse`]'s numbers get for free: it must end in `:<digits>`, name a `.rs` file, carry no
/// whitespace and be short. A tracer, a corrupted log or a future format change cannot then smuggle
/// a title, a URL or a path out through the one gap in the no-text rule.
pub(crate) fn looks_like_a_source_location(s: &str) -> bool {
    const MAX: usize = 120;
    let Some((file, line)) = s.rsplit_once(':') else {
        return false;
    };
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
    let end = digits
        .find(|c: char| !c.is_ascii_hexdigit())
        .unwrap_or(digits.len());
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

/// The panics std itself raises when an earlier panic cannot proceed — its unwind reached an
/// `extern "C"` frame, or a destructor panicked during cleanup. Each is the same death's second
/// record, never a crash of its own, whenever a panic immediately precedes it.
const PANIC_FOLLOWUPS: &[&str] = &[
    ": panic in a function that cannot unwind",
    ": panic in a destructor during cleanup",
];

/// Every report in a crash log, oldest first, **with a panic and the abort it caused counted once**.
///
/// Faults read fixed-shape identity plus **numbers only**. The `(SIGSEGV)` token in the record is
/// deliberately ignored — the name is re-derived from [`signal_name`]. `reg:` is parsed through a
/// fixed register-name allowlist; `img:` accepts exactly a SHA-1 build id and bounded 32-bit ELF
/// numbers. `at:` and `bin:` remain ignored because they carry local paths.
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
///
/// In practice it is THREE records: the panic cannot leave the `extern "C"` frame, so std raises
/// `panic in a function that cannot unwind` from `core::panicking`, and the hook logs that too.
/// The same positional rule folds it ([`PANIC_FOLLOWUPS`]) into the panic before it.
pub(crate) fn parse(log: &str) -> Vec<Report> {
    parse_seeded(log, None)
}

/// Parse records with the last image marker from the already-watermarked prefix.
fn parse_seeded(log: &str, mut image: Option<ImageIdentity>) -> Vec<Report> {
    let mut out: Vec<Report> = Vec::new();
    for l in log.lines() {
        if l.starts_with("img: ") {
            // An explicit but unreadable marker is a process boundary too: clear rather than
            // attributing this process's later crash to the executable that ran before it.
            image = parse_image(l);
        } else if let Some(rest) = l.strip_prefix("*** SIGNAL ") {
            let Some(f) = parse_fault(rest, l, image.clone()) else {
                continue;
            };
            if f.signal == SIGABRT && matches!(out.last(), Some(Report::Panic(_))) {
                continue; // the panic above it is the report — see the doc
            }
            out.push(Report::Fault(f));
        } else if l.starts_with("reg: ") {
            if let Some(Report::Fault(f)) = out.last_mut() {
                f.registers = parse_registers(l);
                f.registers.lr.get_or_insert(f.lr);
                f.registers.pc.get_or_insert(f.pc);
            }
        } else if l.starts_with("*** RUST PANIC ") {
            if PANIC_FOLLOWUPS.iter().any(|m| l.ends_with(m))
                && matches!(out.last(), Some(Report::Panic(_)))
            {
                continue; // std's own follow-up to the panic above — see the doc
            }
            if let Some(mut p) = parse_panic(l) {
                p.image = image.clone();
                out.push(Report::Panic(p));
            }
        }
    }
    out
}

fn parse_fault(rest: &str, whole: &str, image: Option<ImageIdentity>) -> Option<Fault> {
    let pc = hex_field(whole, "pc=")?;
    let lr = hex_field(whole, "lr=")?;
    Some(Fault {
        signal: rest.split_whitespace().next()?.parse().ok()?,
        addr: hex_field(whole, "addr=").unwrap_or(0),
        pc,
        lr,
        image,
        registers: Registers {
            lr: Some(lr),
            pc: Some(pc),
            ..Registers::default()
        },
    })
}

fn parse_image(line: &str) -> Option<ImageIdentity> {
    if !line.starts_with("img: ") {
        return None;
    }
    let at = line.find("build_id=")? + "build_id=".len();
    let rest = &line[at..];
    let end = rest
        .find(|c: char| !c.is_ascii_hexdigit())
        .unwrap_or(rest.len());
    // This app links `--build-id=sha1`: accepting exactly that shape turns a line read from disk
    // into a binary identity, rather than an arbitrary string channel.
    if end != 40 {
        return None;
    }
    let build_id = rest[..end].to_ascii_lowercase();
    let image_addr = hex_field(line, "image_addr=")?;
    let image_size = hex_field(line, "image_size=")?;
    if image_size == 0 || image_addr > u32::MAX as u64 || image_size > u32::MAX as u64 {
        return None;
    }
    Some(ImageIdentity {
        build_id,
        image_addr,
        image_size,
    })
}

fn last_image(log: &str) -> Option<ImageIdentity> {
    let mut image = None;
    for line in log.lines().filter(|line| line.starts_with("img: ")) {
        image = parse_image(line);
    }
    image
}

fn parse_registers(line: &str) -> Registers {
    Registers {
        r0: hex_field(line, "r0="),
        r1: hex_field(line, "r1="),
        r2: hex_field(line, "r2="),
        r3: hex_field(line, "r3="),
        r4: hex_field(line, "r4="),
        r5: hex_field(line, "r5="),
        r6: hex_field(line, "r6="),
        r7: hex_field(line, "r7="),
        r8: hex_field(line, "r8="),
        r9: hex_field(line, "r9="),
        r10: hex_field(line, "r10="),
        fp: hex_field(line, "fp="),
        ip: hex_field(line, "ip="),
        sp: hex_field(line, "sp="),
        lr: hex_field(line, "lr="),
        pc: hex_field(line, "pc="),
        cpsr: hex_field(line, "cpsr="),
    }
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
        image: None,
    })
}

fn registers_json(r: &Registers) -> serde_json::Value {
    let mut out = serde_json::Map::new();
    for (name, value) in [
        ("r0", r.r0),
        ("r1", r.r1),
        ("r2", r.r2),
        ("r3", r.r3),
        ("r4", r.r4),
        ("r5", r.r5),
        ("r6", r.r6),
        ("r7", r.r7),
        ("r8", r.r8),
        ("r9", r.r9),
        ("r10", r.r10),
        ("fp", r.fp),
        ("ip", r.ip),
        ("sp", r.sp),
        ("lr", r.lr),
        ("pc", r.pc),
        ("cpsr", r.cpsr),
    ] {
        if let Some(value) = value {
            out.insert(
                name.to_string(),
                serde_json::Value::String(format!("0x{value:x}")),
            );
        }
    }
    serde_json::Value::Object(out)
}

/// The Sentry event body for one fault.
///
/// `image_addr` comes from [`super::sentry::image_addr`] — the running binary's own lowest
/// `PT_LOAD`, not zero. Its doc carries the measured A/B that settled why zero is the trap.
///
/// `errors_id` is the crash-report identifier in force when the report is QUEUED (the fault itself
/// left no such record — the tracer is async-signal-safe and writes numbers), attached as
/// `user.id` through the one shared [`super::sentry::attach_user`]. Passed in rather than read
/// here so the preview can build this exact body with a placeholder.
pub(crate) fn sentry_body(
    f: &Fault,
    event_id: &str,
    build_id: &str,
    debug_id: Option<&str>,
    errors_id: Option<&str>,
) -> Vec<u8> {
    let name = signal_name(f.signal);
    let effective_build_id = f
        .image
        .as_ref()
        .map(|i| i.build_id.as_str())
        .unwrap_or(build_id);
    let registers = registers_json(&f.registers);
    let mut body = serde_json::json!({
        "event_id": event_id,
        "platform": "native",
        "level": "fatal",
        "release": concat!("plxnative@", env!("PLX_VERSION")),
        "environment": super::sender::ENVIRONMENT,
        "dist": effective_build_id,
        "sdk": {"name": "plxnative-fallback", "version": env!("PLX_VERSION")},
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
    if !registers.as_object().is_some_and(serde_json::Map::is_empty) {
        body["exception"]["values"][0]["stacktrace"]["registers"] = registers;
    }
    // **No debug image rather than a broken one.** An entry whose `debug_id` matches no uploaded
    // object yields `missing_symbol` and NO error — see `sentry::IMAGE_ADDR`'s measured table — so a
    // build whose id could not be read is better off saying nothing than asserting a pairing that
    // does not exist. The addresses are still in the frames, and `code_id` still names the build.
    if let (Some(did), false) = (debug_id, effective_build_id.is_empty()) {
        let image_addr = f.image.as_ref().map(|i| i.image_addr).unwrap_or_else(|| {
            u64::from_str_radix(super::sentry::image_addr().trim_start_matches("0x"), 16)
                .unwrap_or(0)
        });
        let image_size = f
            .image
            .as_ref()
            .map(|i| i.image_size)
            .unwrap_or_else(super::sentry::image_size);
        body["debug_meta"] = serde_json::json!({"images": [{
            "type": "elf",
            "image_addr": format!("0x{image_addr:x}"),
            "image_size": image_size,
            "code_id": effective_build_id,
            "debug_id": did,
            "code_file": CODE_FILE,
        }]});
    }
    super::sentry::attach_user(&mut body, errors_id);
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
pub(crate) fn panic_sentry_body(
    p: &PanicReport,
    event_id: &str,
    errors_id: Option<&str>,
) -> Vec<u8> {
    let mut body = serde_json::json!({
        "event_id": event_id,
        "platform": "native",
        "level": "fatal",
        "release": concat!("plxnative@", env!("PLX_VERSION")),
        "environment": super::sender::ENVIRONMENT,
        "sdk": {"name": "plxnative-fallback", "version": env!("PLX_VERSION")},
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
    if let Some(image) = &p.image {
        body["dist"] = serde_json::Value::String(image.build_id.clone());
    }
    super::sentry::attach_user(&mut body, errors_id);
    serde_json::to_vec(&body).unwrap_or_default()
}

/// Representative fallback payloads built by the real serializers, with every runtime value
/// replaced afterwards by an explicit placeholder. The consent screen can therefore show both
/// degraded schemas without minting an id or inspecting the crash log before consent.
pub(crate) fn preview_events() -> Vec<(&'static str, Vec<u8>)> {
    let image = ImageIdentity {
        build_id: "0".repeat(40),
        image_addr: 0x10000,
        image_size: 0x1000,
    };
    let all = Some(1);
    let fault = Fault {
        signal: 11,
        addr: 0,
        pc: 0x10010,
        lr: 0x10008,
        image: Some(image.clone()),
        registers: Registers {
            r0: all,
            r1: all,
            r2: all,
            r3: all,
            r4: all,
            r5: all,
            r6: all,
            r7: all,
            r8: all,
            r9: all,
            r10: all,
            fp: all,
            ip: all,
            sp: all,
            lr: all,
            pc: all,
            cpsr: all,
        },
    };
    let mut fault_json: serde_json::Value = serde_json::from_slice(&sentry_body(
        &fault,
        &"0".repeat(32),
        &image.build_id,
        Some("00000000-0000-0000-0000-000000000000"),
        Some(super::native::PREVIEW_USER_ID),
    ))
    .unwrap_or_default();
    fault_json["event_id"] = serde_json::json!("<stable id for this crash-log record>");
    fault_json["dist"] = serde_json::json!("<crashed ELF build id>");
    fault_json["exception"]["values"][0]["value"] = serde_json::json!("SIGSEGV at <fault address>");
    if let Some(frames) = fault_json
        .pointer_mut("/exception/values/0/stacktrace/frames")
        .and_then(serde_json::Value::as_array_mut)
    {
        for (i, frame) in frames.iter_mut().enumerate() {
            frame["instruction_addr"] = serde_json::json!(if i == 0 {
                "<caller address>"
            } else {
                "<fault address>"
            });
        }
    }
    if let Some(registers) = fault_json
        .pointer_mut("/exception/values/0/stacktrace/registers")
        .and_then(serde_json::Value::as_object_mut)
    {
        for (name, value) in registers {
            *value = serde_json::json!(if name == "cpsr" {
                "<flags>"
            } else {
                "<address>"
            });
        }
    }
    if let Some(image) = fault_json.pointer_mut("/debug_meta/images/0") {
        image["image_addr"] = serde_json::json!("<load address>");
        image["image_size"] = serde_json::json!("<mapped bytes>");
        image["code_id"] = serde_json::json!("<ELF build id>");
        image["debug_id"] = serde_json::json!("<ELF debug id>");
    }

    let panic = PanicReport {
        location: "src/example.rs:42".to_string(),
        message_hash: 0,
        image: Some(image),
    };
    let mut panic_json: serde_json::Value = serde_json::from_slice(&panic_sentry_body(
        &panic,
        &"0".repeat(32),
        Some(super::native::PREVIEW_USER_ID),
    ))
    .unwrap_or_default();
    let source = "<validated compile-time source:line>";
    panic_json["event_id"] = serde_json::json!("<stable id for this crash-log record>");
    panic_json["dist"] = serde_json::json!("<crashed ELF build id>");
    panic_json["exception"]["values"][0]["value"] = serde_json::json!(
        "Rust panic at <validated compile-time source:line> (msg <message hash>)"
    );
    panic_json["fingerprint"] = serde_json::json!(["rust-panic", source, "<message hash>"]);
    panic_json["culprit"] = serde_json::json!(source);

    vec![
        (
            "C fault fallback",
            serde_json::to_vec(&fault_json).unwrap_or_default(),
        ),
        (
            "Rust panic fallback",
            serde_json::to_vec(&panic_json).unwrap_or_default(),
        ),
    ]
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
            format!(
                "{build_id}|{seq}|sig{}|{:x}|{:x}|{:x}",
                f.signal, f.addr, f.pc, f.lr
            )
        }
        Report::Panic(p) => format!("{build_id}|{seq}|panic|{}|{:x}", p.location, p.message_hash),
    };
    let a = message_hash(&key);
    let b = message_hash(&format!("{key}|2"));
    format!("{a:016x}{b:016x}")
}

/// How much of the append-only crash log has already been reported — and WHICH log that was.
///
/// An offset belongs to a particular file and prefix, not just a length: `identity` is the
/// `(st_dev, st_ino)` it measured, `prefix_hash` the FNV-1a of its first `reported_bytes` bytes,
/// which also catches a truncate-and-rewrite of the same inode and inode reuse after an unlink.
/// The wire format is 0.6.6's. Both bindings default so a mark from before them (0.6.5, and 0.7
/// before this port, wrote `reported_bytes` alone) still loads; [`resume_from`] treats such a mark
/// as no mark at all, because it cannot say which file it measured.
#[derive(serde::Serialize, serde::Deserialize, Default, Clone, Debug, PartialEq, Eq)]
struct Mark {
    reported_bytes: u64,
    #[serde(default)]
    identity: Option<(u64, u64)>,
    #[serde(default)]
    prefix_hash: Option<u64>,
}

/// One read of the crash log: its bytes and the identity of the descriptor they came from.
#[derive(Default)]
struct Snapshot {
    bytes: Vec<u8>,
    identity: Option<(u64, u64)>,
}

impl Snapshot {
    fn mark(&self) -> Mark {
        Mark {
            reported_bytes: self.bytes.len() as u64,
            identity: self.identity,
            prefix_hash: Some(prefix_hash(&self.bytes)),
        }
    }
}

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;

fn prefix_hash(bytes: &[u8]) -> u64 {
    prefix_hash_from(FNV_OFFSET, bytes)
}

fn prefix_hash_from(seed: u64, bytes: &[u8]) -> u64 {
    bytes
        .iter()
        .fold(seed, |h, b| (h ^ u64::from(*b)).wrapping_mul(0x100_0000_01b3))
}

fn log_path() -> std::path::PathBuf {
    #[cfg(test)]
    if let Some(root) = TEST_ROOT.lock().unwrap().as_ref() {
        return root.join("plxnative-crash.log");
    }
    crate::paths::in_runtime_dir(crate::paths::runtime_file::CRASH)
}

fn mark_paths() -> Vec<std::path::PathBuf> {
    #[cfg(test)]
    if let Some(root) = TEST_ROOT.lock().unwrap().as_ref() {
        return vec![root.join("telemetry-crashmark.json")];
    }
    crate::paths::telemetry_crashmark_candidates()
}

#[cfg(test)]
static TEST_ROOT: std::sync::Mutex<Option<std::path::PathBuf>> = std::sync::Mutex::new(None);

/// Read the crash log's bytes and identity from ONE non-following descriptor
/// (`session::open_owned_regular`). A missing log is a valid empty generation; a symlink, a file
/// somebody else owns, or one larger than `MAX_OWNED_FILE` is an error, and nothing is imported.
fn read_snapshot(path: &std::path::Path) -> std::io::Result<Snapshot> {
    use std::io::Read;
    use std::os::unix::fs::MetadataExt;
    let (file, meta) = match crate::plex::session::open_owned_regular(path) {
        Ok(opened) => opened,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Snapshot::default()),
        Err(e) => return Err(e),
    };
    let max = crate::plex::session::MAX_OWNED_FILE;
    let mut bytes = Vec::new();
    file.take(max + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max {
        return Err(std::io::ErrorKind::InvalidData.into());
    }
    Ok(Snapshot {
        bytes,
        identity: Some((meta.dev(), meta.ino())),
    })
}

/// The mark that cuts off everything the log holds now, WITHOUT the importer's allocation bound: a
/// long-lived install may have more than 4 MiB of local crash diagnostics, and that must not make
/// the owner's opt-in impossible. Streams the file through a fixed buffer, hashing the whole
/// existing prefix.
fn cutoff_mark(path: &std::path::Path) -> std::io::Result<Mark> {
    use std::io::Read;
    use std::os::unix::fs::MetadataExt;
    let (mut file, meta) = match crate::plex::session::open_owned_regular(path) {
        Ok(opened) => opened,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Snapshot::default().mark()),
        Err(e) => return Err(e),
    };
    let mut hash = FNV_OFFSET;
    let mut total = 0u64;
    let mut chunk = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        total = total
            .checked_add(read as u64)
            .ok_or(std::io::ErrorKind::InvalidData)?;
        hash = prefix_hash_from(hash, &chunk[..read]);
    }
    Ok(Mark {
        reported_bytes: total,
        identity: Some((meta.dev(), meta.ino())),
        prefix_hash: Some(hash),
    })
}

/// What one boot does with the crash log's fresh reports and the native envelopes beside them.
///
/// **One process death is one Sentry event, and it is the event that says the most.** A fault
/// (`SIGSEGV`, …) the daemon also caught is sent as the daemon's event: it walked the frames and
/// copied the registers, the log record has two addresses. A panic that aborted is the reverse:
/// the daemon's SIGABRT has one frame, `__libc_do_syscall`, and the panic record is the only
/// thing naming a source line — so the panic is sent and the envelope is dropped. That is the
/// same "the panic is the one kept" rule [`parse`] applies inside the log, applied across the two
/// channels; before it was, every fatal panic in production arrived as a frameless SIGABRT.
///
/// Pairing is one-to-one and in order: each record takes the first unused envelope with the same
/// build id and signal (a panic counts as `SIGABRT`).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Plan {
    /// Per report: `None` to send it, `Some(n)` when native envelope `n` is sent in its place.
    report_native: Vec<Option<usize>>,
    /// Per native envelope: `Some(i)` when panic report `i` is sent in its place.
    native_panic: Vec<Option<usize>>,
}

fn reconcile(reports: &[Report], natives: &[Option<super::native::CrashKey>], current_build_id: &str) -> Plan {
    let mut plan = Plan {
        report_native: vec![None; reports.len()],
        native_panic: vec![None; natives.len()],
    };
    let mut used = vec![false; natives.len()];
    for (i, r) in reports.iter().enumerate() {
        let (image, signal) = match r {
            Report::Fault(f) => (&f.image, f.signal),
            Report::Panic(p) => (&p.image, SIGABRT),
        };
        let build_id = image.as_ref().map(|x| x.build_id.as_str()).unwrap_or(current_build_id);
        let Some(n) = natives.iter().enumerate().position(|(n, key)| {
            !used[n] && key.as_ref().is_some_and(|k| k.build_id == build_id && k.signal == signal)
        }) else {
            continue;
        };
        used[n] = true;
        match r {
            Report::Fault(_) => plan.report_native[i] = Some(n),
            Report::Panic(_) => plan.native_panic[n] = Some(i),
        }
    }
    plan
}

/// The side effects of a recovery pass, behind a seam so their ORDER is testable.
trait Recovery {
    /// Durably queue fresh report `i`.
    fn queue_report(&mut self, i: usize) -> bool;
    /// Durably queue native envelope `n`, leaving the file in place.
    fn append_native(&mut self, n: usize) -> bool;
    fn delete_native(&mut self, n: usize);
    /// Move the crash-log watermark past every fresh byte. `false` when it could not be persisted.
    fn advance_mark(&mut self) -> bool;
}

/// Carry out `plan`. `fresh` is whether the log had unread bytes at all (an empty parse of fresh
/// bytes still advances the mark; no fresh bytes means there is nothing to advance past).
///
/// The order is chosen so that a power cut between ANY two steps leaves neither a lost report nor
/// two events for one death — the log side re-reads with deterministic [`event_id_for`] ids, the
/// native side re-imports with the envelope's own id, and Sentry drops a repeated id:
///
/// 1. Queue the reports being sent. The first failure stops the pass, as it always has.
/// 2. Queue every native envelope no panic replaced, leaving the files on disk.
/// 3. Delete each envelope a *queued* panic replaced — BEFORE the mark moves. Cut after the mark
///    and before this, and the next boot would import the envelope with no log record left to
///    pair it with: two events. Cut here instead, and the panic is re-read under the same id.
/// 4. Advance the mark, if every fresh report is accounted for: queued, or its native winner is.
/// 5. Delete the queued envelopes — a fault's winner only once the mark has provably moved, or the
///    next boot would re-read that fault with nothing left to pair it with.
///
/// An envelope whose replacing panic did not get queued is left untouched for the next boot.
fn execute(plan: &Plan, fresh: bool, fx: &mut impl Recovery) {
    let mut queued = vec![false; plan.report_native.len()];
    for (i, native) in plan.report_native.iter().enumerate() {
        if native.is_some() {
            continue;
        }
        if !fx.queue_report(i) {
            break; // do NOT advance past a record that did not reach the disk
        }
        queued[i] = true;
    }
    let mut appended = vec![false; plan.native_panic.len()];
    for (n, panic) in plan.native_panic.iter().enumerate() {
        if panic.is_none() {
            appended[n] = fx.append_native(n);
        }
    }
    for (n, panic) in plan.native_panic.iter().enumerate() {
        if panic.is_some_and(|i| queued[i]) {
            fx.delete_native(n);
        }
    }
    let complete = plan
        .report_native
        .iter()
        .enumerate()
        .all(|(i, native)| native.map_or(queued[i], |n| appended[n]));
    let marked = fresh && complete && fx.advance_mark();
    for (n, done) in appended.iter().enumerate() {
        let won_a_fault = plan.report_native.contains(&Some(n));
        if *done && (marked || !won_a_fault) {
            fx.delete_native(n);
        }
    }
}

/// **Report every crash the last run left behind, once** — from the crash log and from the native
/// daemon's envelopes together, so the two can be paired (see [`Plan`]).
///
/// Called at boot, after consent is published and before anything else can crash — the process that
/// wrote these records no longer exists, which is the whole reason the log is on disk.
///
/// Three things about the bookkeeping are load-bearing.
///
/// **The mark advances only after the records are durably queued**, never before the enqueue, or a
/// spool write that fails leaves the report both unsent and permanently skipped. [`execute`] owns
/// that order, and the order of every envelope delete around it.
///
/// **Replacement invalidates the offset even when the new log is longer** — a factory reset, a
/// reinstall, or somebody clearing `/tmp`. The mark binds the offset to a file identity and prefix
/// hash ([`resume_from`]), so it is never used to skip records it was not counting; a missing or
/// pre-binding mark conservatively cuts off the existing log instead, so nothing from before a
/// provable consent boundary is sent.
///
/// **No debug image without a build id.** An image entry carrying an empty or wrong `debug_id` does
/// not degrade to an unsymbolicated frame with a warning; it produces `missing_symbol` and no error
/// at all, which is indistinguishable from never having uploaded symbols. Sending no image at least
/// says so.
pub(crate) fn recover_pending() {
    recover_pending_at(&log_path());
}

/// **May this process read crash data at all** — the crash log here, and the native envelopes in
/// `native::read_pending`? Both halves, or nothing:
///
/// * crash-report consent (`consent::allows_errors`) — consent gates collection, not just the send;
/// * a Sentry destination compiled into this build (`sender::has_sentry`). Without one, every
///   report read would be spooled for a send that can never happen, and the watermark would move
///   past the records, so a later build that DOES carry a DSN could never report them.
///
/// A build with no DSN therefore touches nothing: no read, no watermark write, no envelope delete.
/// The log and the envelopes stay exactly as the crashed process left them.
pub(crate) fn may_read_crash_data() -> bool {
    super::consent::allows_errors() && super::sender::has_sentry()
}

fn recover_pending_at(path: &std::path::Path) {
    if !may_read_crash_data() {
        return; // no consent, or nowhere to send — nothing is read and nothing is queued
    }
    let natives = super::native::read_pending();
    // An unreadable log (symlink, foreign owner, over the bound) imports nothing and leaves the mark
    // alone; the native envelopes beside it are still handled.
    let (snapshot, readable) = match read_snapshot(path) {
        Ok(s) => (s, true),
        Err(e) => {
            crate::log(&format!("telemetry: crash log not imported: {:?}", e.kind()));
            (Snapshot::default(), false)
        }
    };
    let bytes = &snapshot.bytes;
    let stored = read_mark();
    let from = resume_from(&snapshot, stored.as_ref());
    let fresh = from < bytes.len();
    if readable && !fresh && stored.as_ref() != Some(&snapshot.mark()) {
        // A conservative cutoff (no usable mark) must be persisted now, or a crash appended before
        // the next boot would fall behind that boot's cutoff too.
        write_mark(&snapshot.mark());
    }
    let reports = if fresh {
        // Lossy on purpose: this file is written by a signal handler and by the panic hook, and one
        // torn multi-byte sequence in it must not cost the whole report.
        let text = String::from_utf8_lossy(&bytes[from..]);
        match last_image(&String::from_utf8_lossy(&bytes[..from])) {
            Some(image) => parse_seeded(&text, Some(image)),
            None => parse(&text),
        }
    } else {
        Vec::new()
    };
    let current_build_id = super::sentry::build_id();
    let keys: Vec<_> = natives.iter().map(|n| n.key.clone()).collect();
    let plan = reconcile(&reports, &keys, current_build_id);

    struct Io<'a> {
        reports: &'a [Report],
        natives: &'a [super::native::PendingNative],
        from: usize,
        mark: Mark,
        current_build_id: &'a str,
        // Read once: every report of this pass belongs to the same decision, and a toggle racing
        // this pass must not split one crash log between two identities.
        errors_id: Option<String>,
        queued: usize,
    }
    impl Recovery for Io<'_> {
        fn queue_report(&mut self, i: usize) -> bool {
            let r = &self.reports[i];
            let bid = match r {
                Report::Fault(f) => f.image.as_ref(),
                Report::Panic(p) => p.image.as_ref(),
            }
            .map(|x| x.build_id.as_str())
            .unwrap_or(self.current_build_id);
            let did = super::sentry::debug_id(bid);
            let event_id = event_id_for(bid, self.from + i, r);
            let body = match r {
                Report::Fault(f) => {
                    sentry_body(f, &event_id, bid, did.as_deref(), self.errors_id.as_deref())
                }
                Report::Panic(p) => panic_sentry_body(p, &event_id, self.errors_id.as_deref()),
            };
            let ok = super::spool::append(&super::queue::Record {
                category: super::queue::Category::Errors,
                dest: super::queue::Dest::Sentry,
                event_id,
                body,
            });
            self.queued += usize::from(ok);
            ok
        }
        fn append_native(&mut self, n: usize) -> bool {
            self.natives[n].append()
        }
        fn delete_native(&mut self, n: usize) {
            self.natives[n].delete();
        }
        fn advance_mark(&mut self) -> bool {
            write_mark(&self.mark)
        }
    }
    let mut io = Io {
        reports: &reports,
        natives: &natives,
        from,
        mark: snapshot.mark(),
        current_build_id,
        errors_id: super::consent::errors_id(),
        queued: 0,
    };
    execute(&plan, fresh, &mut io);

    let native_wins = plan.report_native.iter().filter(|n| n.is_some()).count();
    let panic_wins = plan.native_panic.iter().filter(|p| p.is_some()).count();
    if !reports.is_empty() || !natives.is_empty() {
        crate::log(&format!(
            "telemetry: crash log had {} report(s), queued {}, native envelopes {}, native_wins={native_wins}, panic_wins={panic_wins}, symbols={}",
            reports.len(),
            io.queued,
            natives.len(),
            if reports.iter().all(|r| match r {
                Report::Fault(f) => f.image.as_ref().is_some_and(|i| super::sentry::debug_id(&i.build_id).is_some()),
                Report::Panic(_) => true,
            }) { "yes" } else { "partial/no" }
        ));
    }
}

/// Make crash consent prospective. When error reporting is switched on, faults already present in
/// the append-only local log belong to the period in which no upload was authorised. Advancing the
/// private watermark before publishing the new consent keeps those local diagnostics local while
/// allowing the next crash to be reported normally. The cutoff is bound to the log it measured
/// (see [`Mark`]) and is not limited by the importer's read bound, so an oversized local log
/// cannot make the opt-in impossible.
pub(crate) fn discard_pending_before_opt_in() -> bool {
    match cutoff_mark(&log_path()) {
        Ok(mark) => write_mark(&mark),
        Err(e) => {
            crate::log(&format!("telemetry: crash log cutoff not taken: {:?}", e.kind()));
            false
        }
    }
}

/// Where in the crash log to start reading, given what was read and the stored watermark.
///
/// Pure, because the interesting cases cannot be produced on demand. The offset is honoured only
/// for the file it measured: same `(dev, ino)` and the same bytes up to it. **Any mismatch means
/// the file was replaced or rewritten** — a reinstall, a factory reset, somebody clearing `/tmp` —
/// even when the new log is LONGER than the offset, so the log is read from the start rather than
/// skipping the first N bytes of a brand-new log (re-reporting is bounded by the deterministic
/// [`event_id_for`], which Sentry dedupes). **No usable mark** — none, unreadable, or one from
/// before the binding existed — is conservative the other way: everything already in the log
/// predates any cutoff this process can prove, so it stays local.
fn resume_from(snapshot: &Snapshot, mark: Option<&Mark>) -> usize {
    let Some(mark) = mark.filter(|m| m.prefix_hash.is_some()) else {
        return snapshot.bytes.len();
    };
    let Ok(offset) = usize::try_from(mark.reported_bytes) else {
        return 0;
    };
    if snapshot.identity != mark.identity
        || offset > snapshot.bytes.len()
        || Some(prefix_hash(&snapshot.bytes[..offset])) != mark.prefix_hash
    {
        crate::log("telemetry: crash log is not the one the watermark measured — reading it from the start");
        return 0;
    }
    offset
}

fn read_mark() -> Option<Mark> {
    mark_paths()
        .iter()
        .filter_map(|p| crate::plex::session::read_owned_regular(p))
        .find_map(|b| serde_json::from_slice::<Mark>(&b).ok())
}

fn write_mark(mark: &Mark) -> bool {
    let Ok(json) = serde_json::to_vec(mark) else {
        return false;
    };
    let stored = mark_paths()
        .iter()
        .any(|p| crate::plex::session::write_atomic(p, &json).is_ok());
    if !stored {
        // Loud, because the consequence is re-reporting the same crash on every boot until it
        // succeeds — bounded by the deterministic `event_id`, which Sentry dedupes, but still a
        // request per launch that says nothing new.
        crate::log("telemetry: could not persist the crash watermark to ANY candidate path");
    }
    stored
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
    const REC: &str = "\nimg: build_id=11223344556677889900aabbccddeeff00112233 image_addr=0x10000 image_size=0x5f0000\n\
                       *** SIGNAL 11 (SIGSEGV) addr=0x0 pc=0x88ef8 lr=0x4bccd0\n\
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
        assert!(matches!(
            &f[0],
            Report::Fault(Fault {
                signal: 11,
                addr: 0,
                pc: 0x88ef8,
                lr: 0x4bccd0,
                ..
            })
        ));
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
        let two = format!(
            "{REC}some unrelated line\n{}",
            REC.replace("SIGNAL 11", "SIGNAL 6")
        );
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
        assert!(
            parse("*** SIGNAL 11 (SIGSEGV) addr=0x0\n").is_empty(),
            "no pc/lr"
        );
        assert!(
            parse("*** SIGNAL  (SIGSEGV) addr=0x0 pc=0x1 lr=0x2\n").is_empty(),
            "no number"
        );
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
        let body = String::from_utf8(sentry_body(
            &f,
            "e".repeat(32).as_str(),
            "bid",
            Some("did"),
            None,
        ))
        .expect("utf-8");
        for leaked in [
            "/lib/libc.so.6",
            "/media/developer",
            "r-xp",
            "sp=",
            "b6f00000",
            "reg:",
            "at:",
            "bin:",
        ] {
            assert!(
                !body.contains(leaked),
                "the crash log's text reached the wire: {leaked}"
            );
        }
        // …and the fault itself is still fully described, by numbers.
        assert!(
            body.contains("SIGSEGV"),
            "from signal_name, not from the file"
        );
        assert!(body.contains("0x88ef8") && body.contains("0x4bccd0"));
        assert!(
            body.contains("\"cpsr\":\"0x60000010\""),
            "fixed key + numeric value survive"
        );
        assert!(body.contains(super::super::sentry::image_addr()));
    }

    /// **Both fallback shapes carry the crash-report id the same way, and carry nothing when there
    /// is none.** The panic body is the one most likely to be edited without the fault body, and
    /// this is what keeps the two from drifting apart.
    #[test]
    fn both_fallback_bodies_carry_exactly_the_crash_report_id_or_no_user_at_all() {
        let f = only_fault(REC);
        let p = PanicReport {
            location: "src/example.rs:42".into(),
            message_hash: 7,
            image: None,
        };
        let id = "0123456789abcdef0123456789abcdef";
        for body in [
            sentry_body(&f, "e", "bid", Some("did"), Some(id)),
            panic_sentry_body(&p, "e", Some(id)),
        ] {
            let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(v["user"], serde_json::json!({"id": id}));
        }
        for body in [
            sentry_body(&f, "e", "bid", Some("did"), None),
            panic_sentry_body(&p, "e", None),
        ] {
            let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert!(
                v.get("user").is_none(),
                "an absent id must leave no user key"
            );
        }
    }

    /// The report belongs to the binary that CRASHED, not to whichever binary happens to read the
    /// append-only log on the next launch. A deploy between those two moments is ordinary during
    /// development and makes current-process build identity actively wrong.
    #[test]
    fn a_fault_keeps_the_crashed_binarys_image_identity_and_registers() {
        let f = only_fault(REC);
        let parsed = format!("{f:?}");
        assert!(
            parsed.contains("11223344556677889900aabbccddeeff00112233"),
            "build id was ignored"
        );
        assert_eq!(
            f.registers.sp,
            Some(0xbe8f1a90),
            "stack pointer was ignored"
        );
        assert_eq!(f.registers.cpsr, Some(0x60000010), "CPSR was ignored");
    }

    /// Sentry's native image schema needs the mapped image size, while triage needs the build
    /// label and environment without opening a second dashboard. These are assertions over the
    /// literal event body, where an omitted field is otherwise accepted silently by ingest.
    #[test]
    fn a_fault_event_carries_complete_native_metadata() {
        let body = sentry_body(
            &only_fault(REC),
            &"e".repeat(32),
            "11223344556677889900aabbccddeeff00112233",
            Some("44332211-6655-8877-9900-aabbccddeeff"),
            Some("0123456789abcdef0123456789abcdef"),
        );
        let v: serde_json::Value = serde_json::from_slice(&body).expect("event json");
        assert_eq!(
            v["user"],
            serde_json::json!({"id": "0123456789abcdef0123456789abcdef"}),
            "the crash-report id rides as user.id and nothing else"
        );
        assert_eq!(v["environment"], super::super::sender::ENVIRONMENT);
        assert_eq!(
            v["release"],
            concat!("plxnative@", env!("PLX_VERSION"))
        );
        assert_eq!(v["debug_meta"]["images"][0]["image_size"], 0x5f0000);
        assert_eq!(
            v["exception"]["values"][0]["stacktrace"]["registers"]["sp"],
            "0xbe8f1a90"
        );
    }

    /// The image path is a constant, so the report does not say WHICH of the two install prefixes
    /// this person used — a fact about them rather than about the crash.
    #[test]
    fn the_reported_image_path_says_nothing_about_the_install() {
        let body =
            String::from_utf8(sentry_body(&only_fault(REC), "e", "b", Some("d"), None)).unwrap();
        assert!(body.contains("plxnative"));
        assert!(
            !body.contains("/media/"),
            "the install prefix must not be reported"
        );
    }

    /// **A panic's MESSAGE is never carried** — only where it happened and a hash that groups it.
    /// A Rust panic message routinely contains a path or an interpolated value; `unwrap()` on an
    /// io error prints the filename.
    #[test]
    fn a_panic_report_carries_no_message() {
        let msg = "called `Result::unwrap()` on an `Err` value: /media/internal/Films/Dune.mkv";
        let r = PanicReport {
            location: "src/ff.rs:1204".into(),
            message_hash: message_hash(msg),
            image: None,
        };
        let rendered = format!("{r:?}");
        assert!(!rendered.contains("Dune"), "a title reached the report");
        assert!(!rendered.contains("/media/"), "a path reached the report");
        assert!(rendered.contains("src/ff.rs:1204"), "the location is kept");
    }

    /// The exact follow-up record the dev set wrote after `crashtest=panic` (2026-09-19).
    const CANNOT_UNWIND: &str = "*** RUST PANIC [?] at /rustup/toolchains/nightly-aarch64-apple-darwin/\
         lib/rustlib/src/rust/library/core/src/panicking.rs:225: panic in a function that cannot unwind";
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
        assert_eq!(
            r.len(),
            1,
            "a panic and its abort were reported as {} events",
            r.len()
        );
        assert!(matches!(r[0], Report::Panic(_)));
    }

    /// **…and on this toolchain it writes THREE**, which the test above never had. The panic cannot
    /// leave the `extern "C"` frame, so std raises a SECOND panic from `core::panicking` —
    /// `panic in a function that cannot unwind` — and the hook logs that one too, before the abort.
    /// Measured on the dev set (webOS 4.10.2, `crashtest=panic`, 2026-09-19): one death, two panic
    /// events in Sentry. The follow-up says nothing the first one did not; the first says WHERE.
    #[test]
    fn a_panic_its_cannot_unwind_followup_and_the_abort_are_one_report() {
        let abrt = REC.replace("SIGNAL 11", "SIGNAL 6");
        let log = format!("{PANIC_LINE}\n{CANNOT_UNWIND}\n{abrt}");
        let r = parse(&log);
        assert_eq!(r.len(), 1, "one death reported as {} events", r.len());
        let Report::Panic(p) = &r[0] else { panic!("expected the panic") };
        assert_eq!(p.location, "src/ff.rs:1204", "the follow-up displaced the panic that says WHERE");
    }

    /// The follow-up coalesces only onto the panic it follows. Standing alone it is still a crash.
    #[test]
    fn a_cannot_unwind_record_with_no_panic_before_it_is_still_reported() {
        assert_eq!(parse(CANNOT_UNWIND).len(), 1);
        let log = format!("{PANIC_LINE}\n{REC}{CANNOT_UNWIND}");
        assert_eq!(parse(&log).len(), 3, "only the ADJACENT follow-up coalesces");
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
        assert_eq!(
            a,
            event_id_for("abc123", 0, r),
            "a re-read produced a different id"
        );
        assert_eq!(a.len(), 32, "Sentry wants 32 hex characters");
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        // …and the things that make two reports DIFFERENT reports all move it.
        assert_ne!(
            a,
            event_id_for("def456", 0, r),
            "another build is another report"
        );
        assert_ne!(
            a,
            event_id_for("abc123", 1, r),
            "the same fault twice is two reports"
        );
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
        assert!(looks_like_a_source_location(
            "rust-modules/src/player/pump.rs:12"
        ));
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
        let Report::Panic(p) = &parse(PANIC_LINE)[0] else {
            panic!("expected a panic")
        };
        let body = String::from_utf8(panic_sentry_body(p, &"e".repeat(32), None)).expect("utf-8");
        assert!(!body.contains("Dune"), "a title reached the wire");
        assert!(!body.contains("/media/"), "a path reached the wire");
        assert!(!body.contains("unwrap"), "the message reached the wire");
        assert!(
            body.contains("src/ff.rs:1204"),
            "the location is what makes it actionable"
        );
        // Grouped on (location, hash), not on the value string — see `panic_sentry_body`.
        assert!(body.contains("fingerprint"));
    }

    /// **A log shorter than the watermark means the file was REPLACED**, so the mark is reset
    /// rather than used to skip bytes it was never counting. Keeping it would drop exactly the
    /// crashes of a freshly reinstalled app.
    #[test]
    fn a_replaced_crash_log_is_read_from_the_start() {
        let mut snapshot = Snapshot { bytes: vec![b'a'; 1024], identity: Some((1, 2)) };
        let mark = snapshot.mark();
        assert_eq!(resume_from(&snapshot, Some(&mark)), 1024, "the ordinary case skips what was reported");
        snapshot.bytes.extend_from_slice(&[b'b'; 3072]);
        assert_eq!(resume_from(&snapshot, Some(&mark)), 1024, "append retains the cutoff");
        snapshot.bytes.truncate(512);
        assert_eq!(resume_from(&snapshot, Some(&mark)), 0, "a shorter log is a new file");
    }

    /// A REPLACED log that has grown past the old offset must not inherit it: the offset belongs
    /// to the file it measured, not to whatever file now has that name.
    #[test]
    fn a_recreated_longer_crash_log_does_not_inherit_the_old_offset() {
        let _g = crate::testlock::serial();
        let root = std::env::temp_dir().join(format!("plx-crash-generation-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("crash.log");
        std::fs::write(&path, REC).unwrap();
        let mark = read_snapshot(&path).unwrap().mark();
        // Keep the old inode alive to make replacement deterministic even on inode-reusing FSs.
        std::fs::rename(&path, root.join("old.log")).unwrap();
        std::fs::write(&path, REC.repeat(2)).unwrap();
        let replacement = read_snapshot(&path).unwrap();
        assert_eq!(resume_from(&replacement, Some(&mark)), 0);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_same_inode_rewrite_and_missing_mark_are_conservative() {
        let mut snapshot = Snapshot { bytes: REC.as_bytes().to_vec(), identity: Some((1, 2)) };
        let mark = snapshot.mark();
        snapshot.bytes[0] = b'x';
        snapshot.bytes.extend_from_slice(REC.as_bytes());
        assert_eq!(resume_from(&snapshot, Some(&mark)), 0, "a changed prefix invalidates the offset");
        assert_eq!(
            resume_from(&snapshot, None),
            snapshot.bytes.len(),
            "without a cutoff existing crashes stay local"
        );
    }

    /// A mark written before the offset was bound to a file (0.6.5 and pre-port 0.7 wrote only
    /// `reported_bytes`) still LOADS, and is honoured conservatively: it cannot say which file it
    /// measured, so everything already in the log stays local.
    #[test]
    fn a_mark_in_the_pre_port_format_still_loads_and_is_conservative() {
        let legacy: Mark = serde_json::from_slice(br#"{"reported_bytes":1024}"#).expect("loads");
        assert_eq!(legacy.reported_bytes, 1024);
        let snapshot = Snapshot { bytes: vec![b'a'; 4096], identity: Some((1, 2)) };
        assert_eq!(resume_from(&snapshot, Some(&legacy)), 4096);
        // And a 0.6.6 mark round-trips with its binding intact.
        let bound = snapshot.mark();
        let json = serde_json::to_vec(&bound).unwrap();
        let back: Mark = serde_json::from_slice(&json).unwrap();
        assert_eq!(resume_from(&snapshot, Some(&back)), 4096);
        let v: serde_json::Value = serde_json::from_slice(&json).unwrap();
        assert!(v["prefix_hash"].is_u64() && v["identity"].is_array(), "0.6.6 wire format: {v}");
    }

    #[test]
    fn an_oversized_local_log_cannot_disable_error_reporting_opt_in() {
        let _g = crate::testlock::serial();
        let root = std::env::temp_dir().join(format!("plx-crash-large-cutoff-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        *TEST_ROOT.lock().unwrap() = Some(root.clone());
        std::fs::write(log_path(), vec![b'x'; 4 * 1024 * 1024 + 1]).unwrap();

        assert!(discard_pending_before_opt_in());
        let mark = read_mark().expect("the complete existing prefix is cut off");
        assert_eq!(mark.reported_bytes, 4 * 1024 * 1024 + 1);
        assert!(read_snapshot(&log_path()).is_err(), "the importer's read stays bounded");

        *TEST_ROOT.lock().unwrap() = None;
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_symlinked_crash_log_is_not_followed() {
        let _g = crate::testlock::serial();
        let root = std::env::temp_dir().join(format!("plx-crash-symlink-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("elsewhere"), REC).unwrap();
        std::os::unix::fs::symlink(root.join("elsewhere"), root.join("crash.log")).unwrap();
        assert!(read_snapshot(&root.join("crash.log")).is_err());
        assert!(read_snapshot(&root.join("absent.log")).is_ok_and(|s| s.bytes.is_empty()));
        let _ = std::fs::remove_dir_all(root);
    }

    /// The watermark commonly lands after a process marker and before that process later crashes.
    /// The fresh slice then starts at `*** SIGNAL`; its identity must be seeded from the prefix.
    #[test]
    fn a_marker_before_the_watermark_still_identifies_the_later_fault() {
        let split = REC.find("*** SIGNAL").unwrap();
        let seed = last_image(&REC[..split]);
        let reports = parse_seeded(&REC[split..], seed);
        let fault = match &reports[0] {
            Report::Fault(f) => f,
            Report::Panic(_) => panic!("expected fault"),
        };
        assert_eq!(
            fault.image.as_ref().map(|i| i.build_id.as_str()),
            Some("11223344556677889900aabbccddeeff00112233")
        );
    }

    #[test]
    fn an_unreadable_new_process_marker_clears_a_stale_identity() {
        let split = REC.find("*** SIGNAL").unwrap();
        let prefix = format!("{}img: unavailable\n", &REC[..split]);
        assert!(last_image(&prefix).is_none());
        let reports = parse_seeded(&REC[split..], last_image(&prefix));
        let Report::Fault(fault) = &reports[0] else {
            panic!("expected fault")
        };
        assert!(fault.image.is_none());
    }

    const BUILD: &str = "11223344556677889900aabbccddeeff00112233";

    fn key(signal: u32) -> Option<super::super::native::CrashKey> {
        Some(super::super::native::CrashKey { build_id: BUILD.to_string(), signal })
    }

    fn panic_report() -> Report {
        let panic_log = format!("{}\n{}", REC.lines().nth(1).unwrap(), PANIC_LINE);
        let panic = parse(&panic_log).remove(0);
        assert!(matches!(panic, Report::Panic(_)));
        panic
    }

    #[test]
    fn one_native_envelope_suppresses_exactly_one_matching_fallback() {
        let fault = parse(REC).remove(0);
        let plan = reconcile(&[fault.clone(), fault], &[key(11)], "different-current-build");
        assert_eq!(plan.report_native, vec![Some(0), None], "one native event hid two faults");
        assert_eq!(plan.native_panic, vec![None]);
    }

    /// **A panic that aborted is reported as the panic, even when the native daemon caught the abort.**
    ///
    /// Unwinding out of an `extern "C"` frame writes the panic line and a `SIGNAL 6`, and the
    /// daemon writes a SIGABRT envelope whose only frame is `__libc_do_syscall`. Letting that
    /// envelope win threw away the one record naming a source location — production had frameless
    /// SIGABRTs grouped on `__libc_do_syscall` (PLX-NATIVE-W) and not one panic in 90 days.
    #[test]
    fn a_panic_is_not_hidden_by_the_native_abort_it_caused() {
        let plan = reconcile(&[panic_report()], &[key(SIGABRT)], "different-current-build");
        assert_eq!(plan.report_native, vec![None], "the native SIGABRT hid the panic that caused it");
        assert_eq!(plan.native_panic, vec![Some(0)], "the frameless SIGABRT was sent as well");
    }

    #[test]
    fn a_panic_does_not_claim_an_envelope_of_another_signal_or_build() {
        let plan = reconcile(&[panic_report()], &[key(11), None], "different-current-build");
        assert_eq!(plan.native_panic, vec![None, None]);
        let mut other = key(SIGABRT);
        other.as_mut().unwrap().build_id = "ffffffffffffffffffffffffffffffffffffffff".into();
        assert_eq!(reconcile(&[panic_report()], &[other], BUILD).native_panic, vec![None]);
    }

    /// Records every side effect in order, failing the steps it is told to fail.
    #[derive(Default)]
    struct Fx {
        log: Vec<String>,
        fail_report: Option<usize>,
        fail_native: Option<usize>,
        fail_mark: bool,
    }
    impl Recovery for Fx {
        fn queue_report(&mut self, i: usize) -> bool {
            self.log.push(format!("queue r{i}"));
            self.fail_report != Some(i)
        }
        fn append_native(&mut self, n: usize) -> bool {
            self.log.push(format!("append n{n}"));
            self.fail_native != Some(n)
        }
        fn delete_native(&mut self, n: usize) {
            self.log.push(format!("delete n{n}"));
        }
        fn advance_mark(&mut self) -> bool {
            self.log.push("mark".into());
            !self.fail_mark
        }
    }

    fn run(plan: &Plan, fresh: bool, mut fx: Fx) -> Vec<String> {
        execute(plan, fresh, &mut fx);
        fx.log
    }

    /// The envelope a panic replaced goes BEFORE the mark moves: a cut between the two re-reads the
    /// panic under the same event id, where the other order would import the envelope with no log
    /// record left to pair it with — two events for one death.
    #[test]
    fn a_replaced_envelope_is_deleted_after_the_panic_is_queued_and_before_the_mark() {
        let plan = reconcile(&[panic_report()], &[key(SIGABRT)], BUILD);
        assert_eq!(run(&plan, true, Fx::default()), ["queue r0", "delete n0", "mark"]);
    }

    #[test]
    fn an_unqueued_panic_leaves_its_envelope_and_the_mark_alone() {
        let plan = reconcile(&[panic_report()], &[key(SIGABRT)], BUILD);
        let fx = Fx { fail_report: Some(0), ..Fx::default() };
        assert_eq!(run(&plan, true, fx), ["queue r0"]);
    }

    /// A fault's native winner is deleted only AFTER the mark: deleted first, a cut would re-read
    /// the fault with nothing left to pair it with and send it as a second event.
    #[test]
    fn a_fault_winner_is_deleted_only_after_the_mark() {
        let fault = parse(REC).remove(0);
        let plan = reconcile(&[fault], &[key(11)], BUILD);
        assert_eq!(run(&plan, true, Fx::default()), ["append n0", "mark", "delete n0"]);

        let fx = Fx { fail_native: Some(0), ..Fx::default() };
        assert_eq!(run(&plan, true, fx), ["append n0"], "a fault skipped for an unqueued winner");

        let fx = Fx { fail_mark: true, ..Fx::default() };
        assert_eq!(run(&plan, true, fx), ["append n0", "mark"], "winner deleted under an unpersisted mark");
    }

    #[test]
    fn a_failed_report_keeps_a_fault_winner_on_disk_but_frees_an_unrelated_envelope() {
        let fault = parse(REC).remove(0);
        let plan = reconcile(&[fault, panic_report()], &[key(11), key(4)], BUILD);
        let fx = Fx { fail_report: Some(1), ..Fx::default() };
        assert_eq!(
            run(&plan, true, fx),
            ["queue r1", "append n0", "append n1", "delete n1"],
            "n0 must survive an unadvanced mark; n1 pairs with nothing and is done"
        );
    }

    /// Nothing fresh in the log — or no log at all — must not strand an envelope.
    #[test]
    fn envelopes_are_queued_with_no_fresh_log() {
        let plan = reconcile(&[], &[key(11), key(SIGABRT)], BUILD);
        assert_eq!(
            run(&plan, false, Fx::default()),
            ["append n0", "append n1", "delete n0", "delete n1"]
        );
        assert_eq!(run(&reconcile(&[], &[], BUILD), true, Fx::default()), ["mark"]);
    }

    /// **A build whose id could not be read sends NO debug image**, rather than one asserting a
    /// pairing that does not exist. An image with a wrong `debug_id` does not degrade to an
    /// unsymbolicated frame with a warning — it yields `missing_symbol` and no error at all, which
    /// is indistinguishable from never having uploaded symbols. See `sentry::IMAGE_ADDR`.
    #[test]
    fn no_debug_image_is_better_than_one_that_matches_nothing() {
        let f = only_fault(REC);
        let with = String::from_utf8(sentry_body(&f, "e", "bid", Some("did"), None)).unwrap();
        assert!(with.contains("debug_meta") && with.contains("\"debug_id\":\"did\""));

        let without = String::from_utf8(sentry_body(&f, "e", "bid", None, None)).unwrap();
        assert!(
            !without.contains("debug_meta"),
            "an image was claimed with no id to pair it"
        );
        // The addresses still travel: an unsymbolicated report is worth far more than none.
        assert!(without.contains("0x88ef8") && without.contains("0x4bccd0"));

        let no_identity = Fault { image: None, ..f };
        let no_build =
            String::from_utf8(sentry_body(&no_identity, "e", "", Some("did"), None)).unwrap();
        assert!(!no_build.contains("debug_meta"));
    }

    /// …and the hash still groups: the same message hashes alike, different ones differ. That is
    /// what makes dropping the text affordable rather than merely safe.
    #[test]
    fn the_hash_groups_identical_panics_and_separates_others() {
        assert_eq!(
            message_hash("index out of bounds"),
            message_hash("index out of bounds")
        );
        assert_ne!(
            message_hash("index out of bounds"),
            message_hash("divide by zero")
        );
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

    /// A build with no Sentry destination must not read the crash log at all: whatever it queued
    /// could never be sent, and a moved watermark would skip those records for good.
    #[test]
    fn a_build_without_a_sentry_dsn_reads_no_crash_data() {
        use super::super::{consent, spool};
        let _g = crate::testlock::serial();
        if super::super::sender::has_sentry() {
            return; // a developer build with a DSN compiled in cannot exercise this branch
        }
        let dir = std::env::temp_dir()
            .join(format!("plxnative-crashreport-nodsn-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("plxnative-crash.log");
        std::fs::write(&log, REC).unwrap();
        let spool_file = dir.join("spool.bin");
        spool::set_test_path(Some(spool_file.clone()));
        let saved = consent::current();
        consent::install(consent::apply(&consent::Consent::default(), true, false, || {
            Some("e".into())
        }));

        recover_pending_at(&log);

        let spooled = std::fs::metadata(&spool_file).map(|m| m.len()).unwrap_or(0);
        consent::install(saved.unwrap_or_default());
        spool::set_test_path(None);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            spooled, 0,
            "a build with no Sentry DSN read the crash log and spooled {spooled} bytes it can never send"
        );
    }
}
