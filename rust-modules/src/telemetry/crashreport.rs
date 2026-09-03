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

/// How much of the append-only crash log has already been reported.
#[derive(serde::Serialize, serde::Deserialize, Default)]
struct Mark {
    reported_bytes: u64,
}

fn consume_native_match(
    keys: &mut Vec<super::native::CrashKey>,
    report: &Report,
    current_build_id: &str,
) -> bool {
    let (build_id, signal) = match report {
        Report::Fault(f) => (
            f.image
                .as_ref()
                .map(|x| x.build_id.as_str())
                .unwrap_or(current_build_id),
            f.signal,
        ),
        Report::Panic(p) => (
            p.image
                .as_ref()
                .map(|x| x.build_id.as_str())
                .unwrap_or(current_build_id),
            SIGABRT,
        ),
    };
    let Some(pos) = keys
        .iter()
        .position(|key| key.build_id == build_id && key.signal == signal)
    else {
        return false;
    };
    keys.remove(pos);
    true
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
pub(crate) fn report_pending(native_crashes: &[super::native::CrashKey]) {
    if !super::consent::allows_errors() {
        return; // no consent for this category — nothing is read and nothing is queued
    }
    let path = crate::paths::in_runtime_dir("plxnative-crash.log");
    let Ok(bytes) = std::fs::read(&path) else {
        return;
    };

    let from = resume_from(bytes.len() as u64, read_mark().reported_bytes) as usize;
    if from >= bytes.len() {
        return;
    }
    // Lossy on purpose: this file is written by a signal handler and by the panic hook, and one
    // torn multi-byte sequence in it must not cost the whole report.
    let fresh = String::from_utf8_lossy(&bytes[from..]);
    let seed = last_image(&String::from_utf8_lossy(&bytes[..from]));
    let reports = match seed {
        Some(image) => parse_seeded(&fresh, Some(image)),
        None => parse(&fresh),
    };
    if reports.is_empty() {
        write_mark(bytes.len() as u64);
        return;
    }
    let current_build_id = super::sentry::build_id();
    // Read once: every report of this pass belongs to the same decision, and a toggle racing this
    // loop must not split one crash log between two identities.
    let errors_id = super::consent::errors_id();
    let mut native_crashes = native_crashes.to_vec();
    let mut queued = 0usize;
    let mut native_wins = 0usize;
    for (i, r) in reports.iter().enumerate() {
        let bid = match r {
            Report::Fault(f) => f
                .image
                .as_ref()
                .map(|x| x.build_id.as_str())
                .unwrap_or(current_build_id),
            Report::Panic(p) => p
                .image
                .as_ref()
                .map(|x| x.build_id.as_str())
                .unwrap_or(current_build_id),
        };
        if consume_native_match(&mut native_crashes, r, current_build_id) {
            native_wins += 1;
            continue;
        }
        let did = super::sentry::debug_id(bid);
        let event_id = event_id_for(bid, from + i, r);
        let body = match r {
            Report::Fault(f) => {
                sentry_body(f, &event_id, bid, did.as_deref(), errors_id.as_deref())
            }
            Report::Panic(p) => panic_sentry_body(p, &event_id, errors_id.as_deref()),
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
        "telemetry: crash log had {} report(s), queued {queued}, native_wins={native_wins}, symbols={}",
        reports.len(),
        if reports.iter().all(|r| match r {
            Report::Fault(f) => f.image.as_ref().is_some_and(|i| super::sentry::debug_id(&i.build_id).is_some()),
            Report::Panic(_) => true,
        }) { "yes" } else { "partial/no" }
    ));
    if queued + native_wins == reports.len() {
        write_mark(bytes.len() as u64);
    }
}

/// Make crash consent prospective. When error reporting is switched on, faults already present in
/// the append-only local log belong to the period in which no upload was authorised. Advancing the
/// private watermark before publishing the new consent keeps those local diagnostics local while
/// allowing the next crash to be reported normally.
pub(crate) fn discard_pending_before_opt_in() {
    let path = crate::paths::in_runtime_dir("plxnative-crash.log");
    let Ok(meta) = std::fs::metadata(path) else {
        return;
    };
    write_mark(meta.len());
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
        crate::log(
            "telemetry: crash log is shorter than the watermark — reading it from the start",
        );
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
    let Ok(json) = serde_json::to_vec(&Mark { reported_bytes }) else {
        return;
    };
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
        assert_eq!(
            resume_from(4096, 1024),
            1024,
            "the ordinary case skips what was reported"
        );
        assert_eq!(resume_from(512, 1024), 0, "a shorter log is a new file");
        assert_eq!(resume_from(1024, 1024), 1024, "nothing new is not a reset");
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

    #[test]
    fn one_native_envelope_suppresses_exactly_one_matching_fallback() {
        let build_id = "11223344556677889900aabbccddeeff00112233";
        let fault = parse(REC).remove(0);
        let mut keys = vec![super::super::native::CrashKey {
            build_id: build_id.to_string(),
            signal: 11,
        }];
        assert!(consume_native_match(
            &mut keys,
            &fault,
            "different-current-build"
        ));
        assert!(keys.is_empty());
        assert!(
            !consume_native_match(&mut keys, &fault, build_id),
            "one native event hid two faults"
        );

        let panic_log = format!("{}\n{}", REC.lines().nth(1).unwrap(), PANIC_LINE);
        let panic = parse(&panic_log).remove(0);
        keys.push(super::super::native::CrashKey {
            build_id: build_id.to_string(),
            signal: SIGABRT,
        });
        assert!(consume_native_match(
            &mut keys,
            &panic,
            "different-current-build"
        ));
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
}
