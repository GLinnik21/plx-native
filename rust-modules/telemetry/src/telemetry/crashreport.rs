//! **The crash channel, and the guarantee that it carries no text.**
//!
//! Reads the append-only crash log the C tracer writes (`src/crashtrace.c`), turns each fault
//! record, and each `*** FREEZE` record (a main-thread stall the process survived: addresses and a
//! fixed duration bucket, never raw milliseconds), into a Sentry event, and reports it on the next launch — because the process that wrote
//! it no longer exists, which is the whole reason the log is on disk and not in memory.
//!
//! # NOTHING FROM THE LOG'S TEXT REACHES THE WIRE
//!
//! This is a decision, taken deliberately after the alternative was considered and rejected: the
//! app does not send its logs, and the crash channel does not carry breadcrumbs, messages, or any
//! string lifted out of a file.
//!
//! It is enforced structurally rather than by care. [`parse`] reads **numbers only** — the signal
//! number, `addr`, `pc`, `lr`, the registers and the crash's wall-clock second (`time:`, digits
//! only, sent as the event's `timestamp` only inside [`crash_timestamp`]'s window) — and the signal's NAME is then derived from
//! [`signal_name`], a fixed table in this file. So even the one human-readable token in the record
//! (`SIGSEGV`) reaches Sentry as a `&'static str` this crate owns, not as bytes that happened to be
//! in a file. A tracer that one day wrote something unexpected there could not smuggle it out.
//!
//! **One exception, narrowed to the point of being a number's label:** the `at:` lines name the
//! library holding `pc` or `lr`, and its file name (never its folder) is sent so an unsymbolicated
//! crash in `libwayland-client.so.0` says so. [`parse_module`] lets a line through only if its
//! path is absolute, uses `[A-Za-z0-9._+-]` and no `..`, and its mapping holds the fault's own
//! `pc` or `lr` and is executable; the path is otherwise used only to open the file for its build
//! id, and only if it is still the file (device and inode) that was mapped when the process died.
//! A library with no build id (the firmware's glibc has none) is still named, with its load address
//! and size, a zero `debug_id` and no `code_id`.
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

use plx_base::task::watchdog::FreezeBucket;

/// The one place a signal number becomes a name. See the module doc: this exists so the name on the
/// wire comes from a table this crate owns rather than from bytes in a file.
pub fn signal_name(sig: u32) -> &'static str {
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

/// One fault as numeric machine state plus a strictly validated ELF identity. The build id
/// `String` is exactly 40 lowercase hex digits parsed from our own startup marker. The only other
/// text is each [`ModuleMapping`]'s path, which passed [`parse_module`]'s allowlist and is kept
/// solely to open the file for its build id; of it only the basename is ever sent
/// ([`ModuleMapping::file_name`]). No signal text and no other part of a path is retained.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Fault {
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
    /// The mapped modules that contain `pc` or `lr`, from the record's `at:` lines.
    pub modules: Vec<ModuleMapping>,
    /// Unix seconds the tracer stamped on the record (`time:` line), as read. NOT yet checked
    /// against the clock: [`crash_timestamp`] does that when a body is built.
    pub crash_time: Option<u64>,
}

/// One `/proc/self/maps` line the tracer kept because it contains the fault's `pc` or `lr`.
///
/// **The path is kept to OPEN the file, never to send it.** [`module_images`] reads the build id
/// from it; the body carries the basename ([`ModuleMapping::file_name`]) and numbers. The path has
/// passed [`parse_module`]'s allowlist (absolute, a small character set, no `..`, no spaces), so a
/// corrupted log cannot point the build-id read at anything but a plain absolute path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleMapping {
    path: String,
    pub lo: u64,
    pub hi: u64,
    /// File offset of the mapping, so the module's load base is `lo - offset`.
    pub offset: u64,
    /// The `major:minor` of the filesystem the kernel mapped the file from, as the maps line
    /// printed it (both hexadecimal).
    dev: (u32, u32),
    /// The mapped file's inode (decimal in the maps line). Together with `dev` this names the
    /// file that CRASHED, which is not necessarily the one at `path` by the time of the next launch.
    inode: u64,
}

impl ModuleMapping {
    /// The last path component. The only part of the path that may leave the device.
    pub fn file_name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or("")
    }
}

/// A mapped module identified well enough for Sentry to list it in `debug_meta.images`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleImage {
    pub file: String,
    pub image_addr: u64,
    pub image_size: u64,
    /// `None` for a module with no GNU build id: it is still named, with a zero `debug_id`.
    pub code_id: Option<String>,
    pub debug_id: String,
}

/// Why a crash was reported from the append-only log and not from the native daemon's envelope.
///
/// **Only what [`reconcile`] can tell apart is here.** It sees the pending envelopes and the log's
/// reports, nothing else, so it can say "the daemon left nothing" versus "it left something that
/// is not this crash" and cannot say whether the daemon was absent, timed out, or was refused by
/// consent: none of those leaves a record the importer reads. A rejected (malformed or oversized)
/// envelope is deleted by `native::read_pending` before reconcile runs, so it is not
/// distinguishable here either: it is reported as [`NativePath::NoEnvelope`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativePath {
    /// No usable native envelope was pending: the daemon left none, or the one it left was
    /// rejected and deleted before the comparison.
    NoEnvelope,
    /// Envelopes were pending, none of them this crash's build and signal.
    EnvelopeUnmatched,
}

impl NativePath {
    pub const fn as_str(self) -> &'static str {
        match self {
            NativePath::NoEnvelope => "no_envelope",
            NativePath::EnvelopeUnmatched => "envelope_unmatched",
        }
    }
}

/// What a fallback fault event carries beyond the fault itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FaultExtra {
    pub native_path: NativePath,
    pub modules: Vec<ModuleImage>,
}

/// The crash log line that stamps a record with the wall-clock second it was written:
/// `time: <unix seconds>`. The C tracer (`src/crashtrace.c`) writes the same spelling after each
/// `*** SIGNAL` line and the panic hook writes it after each `*** RUST PANIC` line; [`parse`]
/// reads it back. One spelling here so the writer and the reader cannot drift.
pub fn crash_time_record(unix_secs: u64) -> String {
    format!("time: {unix_secs}")
}

/// The wall clock in Unix seconds, `0` if it reads before the epoch. Public so the panic hook
/// (`app/boot.rs`, where a wall-clock read is not allowed) stamps its record with the same clock
/// [`crash_timestamp`] later checks it against.
pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// How far back a crash time is still sent. Sentry drops an event whose `timestamp` is older than
/// 30 days outright, which would cost a crash that today lands (stamped with its receipt time), so
/// an older crash is sent without one; 25 days leaves room for the queue and a retry.
const MAX_CRASH_AGE_SECS: u64 = 25 * 86_400;
/// Clock skew tolerated into the future. Sentry rejects an event more than about a minute ahead.
const FUTURE_SLACK_SECS: u64 = 60;
/// A clock earlier than this has not been set (a television that has not synced yet reads 1970),
/// so it is no reference to bound a crash time against: nothing is sent.
pub(super) const FIRST_PLAUSIBLE_NOW: u64 = 1_704_067_200; // 2024-01-01T00:00:00Z

/// The crash time to send, or `None` to send no `timestamp` at all (Sentry then stamps the event
/// with its receipt time, which is what happened before the record carried one).
///
/// `recorded` is what the tracer wrote; it was read from a file, so it is held to a window around
/// `now` rather than trusted: not in the future (beyond [`FUTURE_SLACK_SECS`]), not older than
/// [`MAX_CRASH_AGE_SECS`], and never against a clock that was not set. A time outside the window
/// is dropped, not clamped: a made-up time is worse than a missing one.
pub fn crash_timestamp(recorded: Option<u64>, now: u64) -> Option<u64> {
    let t = recorded?;
    (now >= FIRST_PLAUSIBLE_NOW
        && t <= now.saturating_add(FUTURE_SLACK_SECS)
        && t >= now.saturating_sub(MAX_CRASH_AGE_SECS))
    .then_some(t)
}

/// A `time:` line's value: 1 to 12 ASCII digits and nothing else. The window check is
/// [`crash_timestamp`]'s, which needs the clock; this only refuses what is not a number.
fn parse_crash_time(s: &str) -> Option<u64> {
    (!s.is_empty() && s.len() <= 12 && s.bytes().all(|b| b.is_ascii_digit()))
        .then(|| s.parse().ok())
        .flatten()
}

/// The `debug_id` of an image with no build id, spelled as sentry-native spells it (the nil UUID).
/// Sentry finds no debug file for it and still shows the image's `code_file`.
const ZERO_DEBUG_ID: &str = "00000000-0000-0000-0000-000000000000";

/// Resolve each mapped module of `f` to an image entry. `read` returns the head of the file the
/// mapping was made from ([`read_module_head`] refuses a file that is no longer that one); a module
/// whose file cannot be read, or is not an ELF, is left out, like the app's own image.
///
/// **A module with no GNU build id is still named.** The firmware's glibc has none (only
/// `NT_GNU_ABI_TAG` and a `.gnu_debuglink`), so a crash inside it used to name no module at all.
/// It gets what a native event lists for such a library: the basename, the load address and the
/// mapped size, a zero `debug_id` ([`ZERO_DEBUG_ID`]) and no `code_id`. A zero id matches no
/// uploaded object, so Sentry shows the module's name and nothing symbolicates against it: it is
/// attributed, not guessed. A module that has an id is exactly as before.
///
/// The app's own mapping is skipped: [`sentry_body`] already describes it from the identity the
/// startup marker recorded, which is the crashed build's and not whatever is deployed now.
/// `image_addr` is the mapping start less its file offset — the module's load base when the text
/// segment's vaddr equals its file offset, which the GNU linkers' output on ARM satisfies — and
/// the size runs from that base to the end of the mapping that holds the address.
pub fn module_images(f: &Fault, read: impl Fn(&ModuleMapping) -> Option<Vec<u8>>) -> Vec<ModuleImage> {
    images_of_mappings(&f.modules, read)
}

/// [`module_images`] over a list of mappings, for a record that is not a [`Fault`].
fn images_of_mappings(
    modules: &[ModuleMapping],
    read: impl Fn(&ModuleMapping) -> Option<Vec<u8>>,
) -> Vec<ModuleImage> {
    modules
        .iter()
        .filter(|m| m.file_name() != CODE_FILE)
        .filter_map(|m| {
            let head = read(m)?;
            let (code_id, debug_id) = match super::sentry::gnu_build_id(&head) {
                Some(id) => {
                    let debug_id = super::sentry::debug_id(&id)?;
                    (Some(id), debug_id)
                }
                // Only a file that really is an ELF: the dev+inode check says it is the mapped
                // file, and this keeps a truncated or foreign file from being listed as a module.
                None if head.starts_with(b"\x7fELF") => (None, ZERO_DEBUG_ID.to_string()),
                None => return None,
            };
            let image_addr = m.lo.checked_sub(m.offset)?;
            Some(ModuleImage {
                file: m.file_name().to_string(),
                image_addr,
                image_size: m.hi - image_addr,
                code_id,
                debug_id,
            })
        })
        .collect()
}

/// The head of a mapped module's file, for [`module_images`]. A regular file only: the path came
/// out of a log, and opening a FIFO or a device node on the boot path could block it.
///
/// **Only the file that was mapped when the process died.** The build id read here is paired with
/// the addresses the crash recorded, so a file replaced since — an app update that landed before
/// the next launch rewrites the bundled `libav*-plx.so.*` and `libass-plx.so.0` — would give
/// Sentry another build's id for this build's addresses, and it would symbolicate them to
/// confident, wrong functions. The mapping's `dev` and `inode` are compared with the handle that
/// is read (opened once, so there is no window between the check and the read); a mismatch reads
/// nothing and the module is left out, which costs a name and never invents one. Firmware
/// libraries survive until a reboot, which also clears the log, so they still match.
///
/// **One exception, development only:** `make deploy` overwrites the bundled libraries IN PLACE
/// (the same inode), so a crash from the previous deploy's mapping followed by a new deploy passes
/// this check with the new file's build id. A shipped install writes new inodes, so the pairing
/// holds there; a development crash report that straddles a deploy can still be mis-symbolicated.
///
/// Opened `O_NONBLOCK` so a FIFO put in the path's place cannot hold the boot path.
fn read_module_head(m: &ModuleMapping) -> Option<Vec<u8>> {
    use std::io::Read;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(&m.path)
        .ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() || split_dev(meta.dev()) != m.dev || meta.ino() != m.inode {
        return None;
    }
    let mut buf = Vec::new();
    file.take(64 * 1024).read_to_end(&mut buf).ok()?;
    Some(buf)
}

/// `st_dev` as the `(major, minor)` pair `/proc/<pid>/maps` prints for the same filesystem. The
/// kernel prints its internal `MAJOR(dev):MINOR(dev)`; `stat` hands out the glibc encoding of it,
/// which spreads a 12-bit major and a 20-bit minor over 64 bits.
fn split_dev(dev: u64) -> (u32, u32) {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        let major = ((dev >> 8) & 0xfff) | ((dev >> 32) & !0xfff);
        let minor = (dev & 0xff) | ((dev >> 12) & !0xff);
        (major as u32, minor as u32)
    }
    // Hosts that cannot be a target (the macOS simulator): there is no maps file there, so this
    // arm only has to be self-consistent for the tests.
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    {
        ((dev >> 24) as u32, (dev & 0xff_ffff) as u32)
    }
}

/// One `at:` line as a [`ModuleMapping`], or `None` for anything that is not a plain, executable,
/// file-backed mapping with a path this module is willing to open and name:
/// `lo-hi perms offset dev inode path` — the kernel's own `/proc/self/maps` line, which the tracer
/// copies verbatim (`dev` is `major:minor` in hex, `inode` decimal).
///
/// The path is held to an allowlist (absolute, `[A-Za-z0-9._+-]` components, no `..`, no spaces —
/// which also drops `[stack]`, anonymous mappings and the kernel's ` (deleted)` suffix) because it
/// is a string out of a file that is about to be opened and whose last component is about to be
/// sent. Addresses must fit a 32-bit address space, like the app's own `img:` marker.
fn parse_module(line: &str) -> Option<ModuleMapping> {
    let mut fields = line.strip_prefix("at: ")?.split_whitespace();
    let (lo, hi) = fields.next()?.split_once('-')?;
    let lo = u64::from_str_radix(lo, 16).ok()?;
    let hi = u64::from_str_radix(hi, 16).ok()?;
    let perms = fields.next()?.as_bytes();
    let offset = u64::from_str_radix(fields.next()?, 16).ok()?;
    let (major, minor) = fields.next()?.split_once(':')?;
    let dev = (u32::from_str_radix(major, 16).ok()?, u32::from_str_radix(minor, 16).ok()?);
    let inode: u64 = fields.next()?.parse().ok()?;
    let path = fields.next()?;
    if fields.next().is_some() || !plain_module_path(path) {
        return None;
    }
    if lo >= hi || hi > u64::from(u32::MAX) + 1 || offset > lo {
        return None;
    }
    // Executable mappings only: the image base `lo - offset` holds where the text segment's vaddr
    // equals its file offset, and a data segment of the same library would claim the same
    // `code_id` with a different, wrong base.
    if perms.first() != Some(&b'r') || perms.get(2) != Some(&b'x') {
        return None;
    }
    Some(ModuleMapping { path: path.to_string(), lo, hi, offset, dev, inode })
}

fn plain_module_path(path: &str) -> bool {
    const MAX_PATH: usize = 160;
    const MAX_NAME: usize = 64;
    let name = path.rsplit('/').next().unwrap_or("");
    path.starts_with('/')
        && path.len() <= MAX_PATH
        && !name.is_empty()
        && name.len() <= MAX_NAME
        && path
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'+' | b'-' | b'/'))
        && !path.split('/').skip(1).any(|c| c.is_empty() || c == "." || c == "..")
}

/// The three ELF facts needed to pair an address with its debug file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageIdentity {
    pub build_id: String,
    pub image_addr: u64,
    pub image_size: u64,
}

/// ARM's integer register set, parsed through a fixed key allowlist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Registers {
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
pub struct PanicReport {
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
    /// Unix seconds the panic hook stamped on the record (`time:` line), as read; see
    /// [`Fault::crash_time`].
    pub crash_time: Option<u64>,
}

/// A main-thread freeze: the watchdog saw the loop stand still for five seconds in the foreground
/// and probed the thread (`src/crashtrace.c`, `freeze_handler`). **Numbers only, like [`Fault`]**:
/// the program counter, the link register, the walked return addresses, the registers, the second
/// the probe landed, and a duration BUCKET from [`FreezeBucket`]'s fixed table (never the stall's
/// milliseconds). The process did not die, so there is no signal and no fault address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Freeze {
    /// The n-th probe of the process that wrote it; what a `freeze-end:` line names.
    pub seq: u32,
    pub pc: u64,
    pub lr: u64,
    /// The frame-pointer walk's return addresses, nearest first. At most [`MAX_FREEZE_FRAMES`].
    pub frames: Vec<u64>,
    pub image: Option<ImageIdentity>,
    pub registers: Registers,
    /// The mappings holding `pc`, `lr` or a walked frame, at most [`MAX_FREEZE_MODULES`].
    pub modules: Vec<ModuleMapping>,
    pub crash_time: Option<u64>,
    /// [`FreezeBucket::Unrecovered`] until a `freeze-end:` line of the same process says otherwise.
    pub bucket: FreezeBucket,
}

/// The most walked frames read from one record, matching the tracer's own cap
/// (`PLX_MAX_FREEZE_FRAMES`); a longer line is a corrupted one and its tail is dropped.
const MAX_FREEZE_FRAMES: usize = 24;
/// The most mappings kept for one freeze: pc, lr and a short chain of callers across a handful of
/// libraries, not a list anyone browses.
const MAX_FREEZE_MODULES: usize = 12;

/// Whether a string is plausibly one of OUR source locations, and therefore safe to send.
///
/// The location is the one field this module lifts out of the log as text, so it gets the treatment
/// [`parse`]'s numbers get for free: it must end in `:<digits>`, name a `.rs` file, carry no
/// whitespace and be short. A tracer, a corrupted log or a future format change cannot then smuggle
/// a title, a URL or a path out through the one gap in the no-text rule.
pub fn looks_like_a_source_location(s: &str) -> bool {
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
pub fn message_hash(msg: &str) -> u64 {
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
pub enum Report {
    Fault(Fault),
    Panic(PanicReport),
    Freeze(Freeze),
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
/// numbers. `at:` lines that hold the fault's `pc`/`lr` become [`ModuleMapping`]s (see
/// [`parse_module`] for what a path must look like); `bin:` is ignored because it carries a local path.
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
pub fn parse(log: &str) -> Vec<Report> {
    parse_seeded(log, None)
}

/// Parse records with the last image marker from the already-watermarked prefix.
fn parse_seeded(log: &str, mut image: Option<ImageIdentity>) -> Vec<Report> {
    let mut out: Vec<Report> = Vec::new();
    // The first report of the current process. A `freeze-end:` names a per-process probe number,
    // so it may only close a freeze written after the startup marker that began its own process.
    let mut process_start = 0usize;
    // Where each process's reports begin, for the stuck-freeze rule below.
    let mut process_starts: Vec<usize> = vec![0];
    // The report whose own `reg:` line has not been read yet. A `reg:` belongs to the header it
    // directly follows, so any other header (a skipped one included) closes the slot.
    let mut awaiting_regs: Option<usize> = None;
    for l in log.lines() {
        if l.starts_with("img: ") {
            awaiting_regs = None;
            process_starts.push(out.len());
            // An explicit but unreadable marker is a process boundary too: clear rather than
            // attributing this process's later crash to the executable that ran before it.
            image = parse_image(l);
            process_start = out.len();
        } else if let Some(rest) = l.strip_prefix("*** FREEZE ") {
            awaiting_regs = None;
            if let Some(f) = parse_freeze(rest, l, image.clone()) {
                out.push(Report::Freeze(f));
                awaiting_regs = Some(out.len() - 1);
            }
        } else if let Some(rest) = l.strip_prefix("frames:") {
            if let Some(Report::Freeze(f)) = out.last_mut() {
                let room = MAX_FREEZE_FRAMES.saturating_sub(f.frames.len());
                f.frames.extend(parse_frames(rest).into_iter().take(room));
            }
        } else if let Some(rest) = l.strip_prefix("freeze-end: ") {
            if let Some((seq, bucket)) = parse_freeze_end(rest) {
                let open = out[process_start..].iter_mut().rev().find_map(|r| match r {
                    Report::Freeze(f) if f.seq == seq => Some(f),
                    _ => None,
                });
                if let Some(f) = open {
                    f.bucket = bucket;
                }
            }
        } else if let Some(rest) = l.strip_prefix("*** SIGNAL ") {
            awaiting_regs = None;
            let Some(f) = parse_fault(rest, l, image.clone()) else {
                continue;
            };
            if f.signal == SIGABRT && matches!(out.last(), Some(Report::Panic(_))) {
                continue; // the panic above it is the report — see the doc
            }
            out.push(Report::Fault(f));
            awaiting_regs = Some(out.len() - 1);
        } else if l.starts_with("at: ") {
            // Kept only when the mapping holds THIS fault's pc or lr, which also stops a stray
            // `at:` after a skipped record being credited to the fault before it.
            match (out.last_mut(), parse_module(l)) {
                (Some(Report::Fault(f)), Some(m)) => {
                    let holds = |a: u64| (m.lo..m.hi).contains(&a);
                    if (holds(f.pc) || holds(f.lr))
                        && f.modules.len() < 2
                        && !f.modules.iter().any(|x| x.lo == m.lo)
                    {
                        f.modules.push(m);
                    }
                }
                (Some(Report::Freeze(f)), Some(m)) => {
                    let holds = |a: u64| (m.lo..m.hi).contains(&a);
                    if (holds(f.pc) || holds(f.lr) || f.frames.iter().any(|a| holds(*a)))
                        && f.modules.len() < MAX_FREEZE_MODULES
                        && !f.modules.iter().any(|x| x.lo == m.lo)
                    {
                        f.modules.push(m);
                    }
                }
                _ => {}
            }
        } else if l.starts_with("reg: ") {
            match awaiting_regs.take().and_then(|i| out.get_mut(i)) {
                Some(Report::Fault(Fault { registers, pc, lr, .. }))
                | Some(Report::Freeze(Freeze { registers, pc, lr, .. })) => {
                    *registers = parse_registers(l);
                    registers.lr.get_or_insert(*lr);
                    registers.pc.get_or_insert(*pc);
                }
                _ => {}
            }
        } else if let Some(rest) = l.strip_prefix("time: ") {
            // The record's own wall-clock second. Applied to the report just above it and only if
            // that report has none yet, so a stray later line cannot move a recorded time, and a
            // panic's abort (folded into the panic) cannot overwrite the panic's.
            if let Some(t) = parse_crash_time(rest) {
                match out.last_mut() {
                    Some(Report::Fault(f)) => {
                        f.crash_time.get_or_insert(t);
                    }
                    Some(Report::Panic(p)) => {
                        p.crash_time.get_or_insert(t);
                    }
                    Some(Report::Freeze(f)) => {
                        f.crash_time.get_or_insert(t);
                    }
                    None => {}
                }
            }
        } else if l.starts_with("*** RUST PANIC ") {
            awaiting_regs = None;
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
    drop_freezes_that_ended_in_a_crash(out, &process_starts)
}

/// A freeze that never recovered and is followed by a fault or a panic of the SAME process is the
/// crash's own prelude, not a separate hang: the process died stuck or while stuck, and the crash
/// is the report that says where and why. Reported too it would be two events for one death (and a
/// freeze probe that overlapped a crash on another thread would be a bogus one). A freeze from an
/// earlier process, or one that recovered, is kept.
fn drop_freezes_that_ended_in_a_crash(out: Vec<Report>, process_starts: &[usize]) -> Vec<Report> {
    let process_of = |i: usize| process_starts.iter().rposition(|&start| start <= i).unwrap_or(0);
    let mut crashed = vec![false; process_starts.len()];
    for (i, r) in out.iter().enumerate() {
        if matches!(r, Report::Fault(_) | Report::Panic(_)) {
            crashed[process_of(i)] = true;
        }
    }
    out.into_iter()
        .enumerate()
        .filter(|(i, r)| {
            !matches!(r, Report::Freeze(f) if f.bucket == FreezeBucket::Unrecovered && crashed[process_of(*i)])
        })
        .map(|(_, r)| r)
        .collect()
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
        modules: Vec::new(),
        crash_time: None,
    })
}

/// `*** FREEZE seq=<n> pc=0x… lr=0x…`, the line `plx_fmt_freeze` (`src/crashfmt.h`) writes.
/// `seq` is decimal digits and nothing else; anything after the three fields is ignored.
fn parse_freeze(rest: &str, whole: &str, image: Option<ImageIdentity>) -> Option<Freeze> {
    let seq_text = rest.strip_prefix("seq=")?.split_whitespace().next()?;
    if seq_text.is_empty() || seq_text.len() > 9 || !seq_text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let pc = hex_field(whole, "pc=")?;
    let lr = hex_field(whole, "lr=")?;
    Some(Freeze {
        seq: seq_text.parse().ok()?,
        pc,
        lr,
        frames: Vec::new(),
        image,
        registers: Registers { lr: Some(lr), pc: Some(pc), ..Registers::default() },
        modules: Vec::new(),
        crash_time: None,
        bucket: FreezeBucket::Unrecovered,
    })
}

/// A `frames:` line's addresses: every token must be `0x` and 1 to 8 hex digits (the address space
/// is 32-bit), or the whole line is refused, so no token that is not a number is read at all.
fn parse_frames(rest: &str) -> Vec<u64> {
    let mut out = Vec::new();
    for tok in rest.split_whitespace() {
        let Some(digits) = tok.strip_prefix("0x") else { return Vec::new() };
        if digits.is_empty() || digits.len() > 8 || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Vec::new();
        }
        let Ok(a) = u64::from_str_radix(digits, 16) else { return Vec::new() };
        out.push(a);
    }
    out
}

/// `seq=<n> bucket=<word>` of a `freeze-end:` line; the word must be one of the three
/// [`FreezeBucket`] spellings, so a recovery can never carry anything else onto the wire.
fn parse_freeze_end(rest: &str) -> Option<(u32, FreezeBucket)> {
    let mut it = rest.split_whitespace();
    let seq = it.next()?.strip_prefix("seq=")?;
    let bucket = it.next()?.strip_prefix("bucket=")?;
    if it.next().is_some() || seq.is_empty() || seq.len() > 9 || !seq.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((seq.parse().ok()?, FreezeBucket::from_word(bucket)?))
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
        crash_time: None,
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

/// Whether `lr` can be sent as a caller frame: it must lie in an executable mapping the log
/// recorded (an `at:` line, which [`parse_module`] keeps only for `r-x` mappings) or in the app's
/// own image (the startup marker's span, for the case where the app's path did not pass the
/// allowlist). On ARM `lr` is only a return address while the faulting function has not reused it;
/// a signal raised from a library routine can leave a stack address or a scratch value there (a
/// SIGABRT from firmware glibc had `lr == sp`), and a frame at a stack address symbolicates to
/// nothing and reads as a real caller. With no mapping to vouch for it the frame is omitted; the
/// raw register stays in `registers`.
fn lr_is_a_caller(f: &Fault) -> bool {
    let in_module = f.modules.iter().any(|m| (m.lo..m.hi).contains(&f.lr));
    let in_image = f.image.as_ref().is_some_and(|i| {
        i.image_addr
            .checked_add(i.image_size)
            .is_some_and(|end| (i.image_addr..end).contains(&f.lr))
    });
    in_module || in_image
}

/// The `debug_meta.images` of a body built from the log: the app's own image (from the identity the
/// startup marker recorded) and the libraries holding the addresses. Shared by every body this
/// module builds from a record, so two of them cannot disagree about either.
fn debug_images(
    recorded: Option<&ImageIdentity>,
    effective_build_id: &str,
    debug_id: Option<&str>,
    modules: &[ModuleImage],
) -> Vec<serde_json::Value> {
    // **No debug image rather than a broken one, for the app's own image.** An entry whose
    // `debug_id` matches no uploaded object yields `missing_symbol` and NO error — see
    // `sentry::IMAGE_ADDR`'s measured table — so a build whose id could not be read is better off
    // saying nothing than asserting a pairing that does not exist. The addresses are still in the
    // frames, and `code_id` still names the build. (A firmware library without a build id is the
    // deliberate exception, and is added below by `module_images` with a nil `debug_id`.)
    let mut images = Vec::new();
    if let (Some(did), false) = (debug_id, effective_build_id.is_empty()) {
        let image_addr = recorded.map(|i| i.image_addr).unwrap_or_else(|| {
            u64::from_str_radix(super::sentry::image_addr().trim_start_matches("0x"), 16)
                .unwrap_or(0)
        });
        let image_size = recorded.map(|i| i.image_size).unwrap_or_else(super::sentry::image_size);
        images.push(serde_json::json!({
            "type": "elf",
            "image_addr": format!("0x{image_addr:x}"),
            "image_size": image_size,
            "code_id": effective_build_id,
            "debug_id": did,
            "code_file": CODE_FILE,
        }));
    }
    // The libraries holding pc and lr, named by BASENAME only (see [`ModuleMapping`]). Their ids
    // were read from the files, not recorded at crash time like the app's own entry; the read
    // ([`read_module_head`]) only accepts the file whose device and inode the maps line recorded,
    // so a library replaced since (an app update, or a reboot's fresh /tmp) is left out here.
    for m in modules {
        let mut image = serde_json::json!({
            "type": "elf",
            "image_addr": format!("0x{:x}", m.image_addr),
            "image_size": m.image_size,
            "debug_id": m.debug_id,
            "code_file": m.file,
        });
        if let Some(code_id) = &m.code_id {
            image["code_id"] = serde_json::json!(code_id);
        }
        images.push(image);
    }
    images
}

/// The Sentry event body for one fault.
///
/// **The title's "address" is not always an address.** `SIGNAL at 0x…` prints `si_addr`, which for
/// a signal sent by `raise()`/`kill()` (a `SIGABRT` from `abort()`, say) is the SENDER'S PID, not
/// where anything faulted (`SIGABRT at 0x4c1b`). The title is kept as it is on purpose: Sentry
/// normalises the hex when it groups, and changing the string would regroup every existing issue.
///
/// **`timestamp` is the crash's own time**, from the record's `time:` line, and is absent when the
/// record has none or [`crash_timestamp`] rejects it, so Sentry never shows the (later) time the
/// report was imported.
///
/// `image_addr` comes from [`super::sentry::image_addr`] — the running binary's own lowest
/// `PT_LOAD`, not zero. Its doc carries the measured A/B that settled why zero is the trap.
///
/// `errors_id` is the crash-report identifier in force when the report is QUEUED (the fault itself
/// left no such record — the tracer is async-signal-safe and writes numbers), attached as
/// `user.id` through the one shared [`super::sentry::attach_user`]. Passed in rather than read
/// here so the preview can build this exact body with a placeholder.
pub fn sentry_body(
    f: &Fault,
    event_id: &str,
    build_id: &str,
    debug_id: Option<&str>,
    errors_id: Option<&str>,
    extra: &FaultExtra,
) -> Vec<u8> {
    let name = signal_name(f.signal);
    let effective_build_id = f
        .image
        .as_ref()
        .map(|i| i.build_id.as_str())
        .unwrap_or(build_id);
    let registers = registers_json(&f.registers);
    // Oldest first, which is the order Sentry renders bottom-up: lr is the caller, so it goes first
    // and pc — the faulting instruction — ends up on top. lr is a caller only if the log vouches
    // that it points into code ([`lr_is_a_caller`]); otherwise it is left to `registers`.
    let mut frames = Vec::with_capacity(2);
    if lr_is_a_caller(f) {
        frames.push(serde_json::json!({"instruction_addr": format!("0x{:x}", f.lr), "platform": "native"}));
    }
    frames.push(serde_json::json!({"instruction_addr": format!("0x{:x}", f.pc), "platform": "native"}));
    let mut body = serde_json::json!({
        "event_id": event_id,
        "platform": "native",
        "level": "fatal",
        "release": super::release(),
        "environment": super::sender::ENVIRONMENT,
        "dist": effective_build_id,
        "sdk": {"name": "plxnative-fallback", "version": super::app_version()},
        // **The value is built from the NAME and the ADDRESS, both of which this crate owns.** Not
        // the record's own line, which would be one line of code and would make the no-text
        // guarantee depend on what the C tracer happens to write.
        "exception": {"values": [{
            "type": name,
            "value": format!("{name} at 0x{:x}", f.addr),
            "mechanism": {"type": "signalhandler", "handled": false,
                          "meta": {"signal": {"number": f.signal, "name": name}}},
            "stacktrace": {"frames": frames},
        }]},
    });
    if let Some(t) = crash_timestamp(f.crash_time, now_secs()) {
        body["timestamp"] = serde_json::json!(t);
    }
    if !registers.as_object().is_some_and(serde_json::Map::is_empty) {
        body["exception"]["values"][0]["stacktrace"]["registers"] = registers;
    }
    let images = debug_images(f.image.as_ref(), effective_build_id, debug_id, &extra.modules);
    if !images.is_empty() {
        body["debug_meta"] = serde_json::json!({ "images": images });
    }
    // The contexts a native crash carries, which the SDK's scope gives it and this hand-built body
    // never sees — plus the one thing only a fallback can say.
    super::sentry::attach_os_context(&mut body);
    super::sentry::attach_hardware_context(&mut body);
    body["contexts"]["crash"] = serde_json::json!({
        "type": "crash",
        "native_path": extra.native_path.as_str(),
    });
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
///
/// It carries the same `os`/`webos`/`hardware` contexts as [`sentry_body`] (the same two attach
/// functions, so the same closed `native::{OS,WEBOS,HARDWARE}_FIELDS` vocabulary) and none of the
/// fault-only `crash` context. Both are read from the process-wide device snapshot at the time this
/// runs, on the next launch, which is the same television the panic was logged on.
pub fn panic_sentry_body(
    p: &PanicReport,
    event_id: &str,
    errors_id: Option<&str>,
) -> Vec<u8> {
    let mut body = serde_json::json!({
        "event_id": event_id,
        "platform": "native",
        "level": "fatal",
        "release": super::release(),
        "environment": super::sender::ENVIRONMENT,
        "sdk": {"name": "plxnative-fallback", "version": super::app_version()},
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
    // The panic's own time (the hook's `time:` line), under the same bounds as a fault's.
    if let Some(t) = crash_timestamp(p.crash_time, now_secs()) {
        body["timestamp"] = serde_json::json!(t);
    }
    // The same `os`/`webos`/`hardware` contexts the fault fallback carries, through the same two
    // functions. This body is built on the NEXT launch, from the crash log, by
    // [`recover_pending`] — never inside the panic hook — so reading the booted device's
    // snapshot here takes no lock the panicking thread could have held.
    super::sentry::attach_os_context(&mut body);
    super::sentry::attach_hardware_context(&mut body);
    super::sentry::attach_user(&mut body, errors_id);
    serde_json::to_vec(&body).unwrap_or_default()
}

/// Where `addr` sits, as `(file, offset, is_app)`: the file a basename this crate owns or the
/// constant [`CODE_FILE`] for the app's own image. `None` for an address no recorded mapping and no
/// image identity vouches for.
fn freeze_locate(f: &Freeze, addr: u64) -> Option<(String, u64, bool)> {
    if let Some(i) = &f.image {
        if let Some(off) = addr.checked_sub(i.image_addr).filter(|o| *o < i.image_size) {
            return Some((CODE_FILE.to_string(), off, true));
        }
    }
    f.modules.iter().find(|m| (m.lo..m.hi).contains(&addr)).and_then(|m| {
        let off = addr.checked_sub(m.lo.checked_sub(m.offset)?)?;
        Some((m.file_name().to_string(), off, m.file_name() == CODE_FILE))
    })
}

/// The addresses of a freeze's stack, nearest first: `pc`, then `lr` (only when a mapping or the
/// image vouches that it is code, as [`lr_is_a_caller`] does for a fault), then the walked frames,
/// with a repeat of the previous address dropped (the walk's first entry is usually `lr` again).
fn freeze_stack(f: &Freeze) -> Vec<u64> {
    let mut out = vec![f.pc];
    let mut push = |a: u64| {
        if out.last() != Some(&a) {
            out.push(a);
        }
    };
    if freeze_locate(f, f.lr).is_some() {
        push(f.lr);
    }
    for a in &f.frames {
        push(*a);
    }
    out
}

/// The second fingerprint component of a freeze: `<file>+0x<offset>` of the NEAREST frame inside
/// the app's own image, else of the nearest frame in any recorded mapping, else `unmapped`.
///
/// **Why not simply the top frame.** A main thread that is stuck is nearly always parked inside
/// libc (`poll`, `futex`, `nanosleep`), so `pc` names the same library offset whatever the app was
/// waiting for, and every freeze would land in one issue. The first app frame names the call that
/// waited. Never a raw address: an unmapped one reads `unmapped`, since a runtime address is a
/// per-boot value that would split one cause into many issues.
fn freeze_fingerprint_site(f: &Freeze) -> String {
    let located: Vec<_> = freeze_stack(f).iter().filter_map(|a| freeze_locate(f, *a)).collect();
    located
        .iter()
        .find(|(_, _, app)| *app)
        .or_else(|| located.first())
        .map(|(file, off, _)| format!("{file}+0x{off:x}"))
        .unwrap_or_else(|| "unmapped".to_string())
}

/// The Sentry event body for one main-thread freeze.
///
/// Level `error` (the process survived), tag `freeze` with the duration bucket's word, mechanism
/// `freeze` (`handled` once the loop recovered; an unrecovered one is `handled: false`, since the
/// process died or was killed still stuck). **The fingerprint is explicit**, `["freeze", site]`
/// from [`freeze_fingerprint_site`], and carries neither the bucket nor a signal: the same stall
/// lasting 6 s on one television and 40 s on another is one cause, one issue.
///
/// The exception `type` and `value` are built from this crate's own strings and the bucket's fixed
/// word, never from the record's text, like [`sentry_body`]'s.
pub fn freeze_sentry_body(
    f: &Freeze,
    event_id: &str,
    build_id: &str,
    debug_id: Option<&str>,
    errors_id: Option<&str>,
    modules: &[ModuleImage],
) -> Vec<u8> {
    let effective_build_id = f.image.as_ref().map(|i| i.build_id.as_str()).unwrap_or(build_id);
    let word = f.bucket.word();
    let value = if f.bucket == FreezeBucket::Unrecovered {
        "Main thread frozen, no recovery recorded".to_string()
    } else {
        format!("Main thread frozen for {word}")
    };
    // Oldest first, which is the order Sentry renders bottom-up: the walk's outermost frame first,
    // pc (where the thread was parked) last and on top.
    let frames: Vec<_> = freeze_stack(f)
        .iter()
        .enumerate()
        .rev()
        .map(|(i, a)| {
            serde_json::json!({
                "instruction_addr": format!("0x{a:x}"),
                "platform": "native",
                "trust": if i == 0 { "context" } else { "fp" },
            })
        })
        .collect();
    let mut body = serde_json::json!({
        "event_id": event_id,
        "platform": "native",
        "level": "error",
        "release": super::release(),
        "environment": super::sender::ENVIRONMENT,
        "dist": effective_build_id,
        "sdk": {"name": "plxnative-fallback", "version": super::app_version()},
        "tags": {"freeze": word},
        "exception": {"values": [{
            "type": "MainThreadFreeze",
            "value": value,
            "mechanism": {"type": "freeze", "handled": f.bucket != FreezeBucket::Unrecovered},
            "stacktrace": {"frames": frames},
        }]},
        "fingerprint": ["freeze", freeze_fingerprint_site(f)],
    });
    if let Some(t) = crash_timestamp(f.crash_time, now_secs()) {
        body["timestamp"] = serde_json::json!(t);
    }
    let registers = registers_json(&f.registers);
    if !registers.as_object().is_some_and(serde_json::Map::is_empty) {
        body["exception"]["values"][0]["stacktrace"]["registers"] = registers;
    }
    let images = debug_images(f.image.as_ref(), effective_build_id, debug_id, modules);
    if !images.is_empty() {
        body["debug_meta"] = serde_json::json!({ "images": images });
    }
    super::sentry::attach_os_context(&mut body);
    super::sentry::attach_hardware_context(&mut body);
    super::sentry::attach_user(&mut body, errors_id);
    serde_json::to_vec(&body).unwrap_or_default()
}

/// The fallback sample's `hardware.probe`: the four words the real field draws from, per nyx file.
/// The consent screen lists the same string among its translated placeholders.
const FALLBACK_PROBE_PLACEHOLDER: &str = "<ok / no_keys / unreadable / unprobed per nyx file>";

/// Representative fallback payloads built by the real serializers, with every runtime value
/// replaced afterwards by an explicit placeholder. The consent screen can therefore show both
/// degraded schemas without minting an id or inspecting the crash log before consent.
pub fn preview_events() -> Vec<(&'static str, Vec<u8>)> {
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
        modules: Vec::new(),
        // So the real serializer emits the field; the value is replaced by a placeholder below.
        crash_time: Some(now_secs()),
    };
    let mut fault_json: serde_json::Value = serde_json::from_slice(&sentry_body(
        &fault,
        &"0".repeat(32),
        &image.build_id,
        Some("00000000-0000-0000-0000-000000000000"),
        Some(super::native::PREVIEW_USER_ID),
        &FaultExtra {
            native_path: NativePath::NoEnvelope,
            modules: vec![ModuleImage {
                file: "<library file name>".to_string(),
                image_addr: 0x20000,
                image_size: 0x1000,
                code_id: Some("0".repeat(40)),
                debug_id: "00000000-0000-0000-0000-000000000000".to_string(),
            }],
        },
    ))
    .unwrap_or_default();
    // The contexts are the NATIVE preview's own placeholders, so the two shapes cannot disagree and
    // this host's kernel and model are not shown to a person deciding what to share.
    let native: serde_json::Value =
        serde_json::from_slice(&super::native::preview_event()).unwrap_or_default();
    // A native crash event has no `probe` (the SDK's scope cannot set it), but both fallbacks are
    // built through `attach_hardware_context`, which adds it, so each sample must show it.
    let placeholder_contexts = |json: &mut serde_json::Value| {
        for key in ["os", "webos", "hardware"] {
            json["contexts"][key] = native["contexts"][key].clone();
        }
        json["contexts"]["hardware"]["probe"] = serde_json::json!(FALLBACK_PROBE_PLACEHOLDER);
    };
    placeholder_contexts(&mut fault_json);

    fault_json["event_id"] = serde_json::json!("<stable id for this crash-log record>");
    fault_json["dist"] = serde_json::json!("<crashed ELF build id>");
    fault_json["timestamp"] = serde_json::json!("<crash time>");
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
    // Entry 0 is the app's own image, entry 1 a library holding pc or lr.
    for i in 0..2 {
        if let Some(image) = fault_json.pointer_mut(&format!("/debug_meta/images/{i}")) {
            image["image_addr"] = serde_json::json!("<load address>");
            image["image_size"] = serde_json::json!("<mapped bytes>");
            image["code_id"] = serde_json::json!("<ELF build id>");
            image["debug_id"] = serde_json::json!("<ELF debug id>");
        }
    }

    let panic = PanicReport {
        location: "src/example.rs:42".to_string(),
        message_hash: 0,
        image: Some(image),
        crash_time: Some(now_secs()),
    };
    let mut panic_json: serde_json::Value = serde_json::from_slice(&panic_sentry_body(
        &panic,
        &"0".repeat(32),
        Some(super::native::PREVIEW_USER_ID),
    ))
    .unwrap_or_default();
    placeholder_contexts(&mut panic_json);
    let source = "<validated compile-time source:line>";
    panic_json["event_id"] = serde_json::json!("<stable id for this crash-log record>");
    panic_json["dist"] = serde_json::json!("<crashed ELF build id>");
    panic_json["timestamp"] = serde_json::json!("<crash time>");
    panic_json["exception"]["values"][0]["value"] = serde_json::json!(
        "Rust panic at <validated compile-time source:line> (msg <message hash>)"
    );
    panic_json["fingerprint"] = serde_json::json!(["rust-panic", source, "<message hash>"]);
    panic_json["culprit"] = serde_json::json!(source);

    vec![
        (
            plx_platform::i18n::msg::core_preview_fault(),
            serde_json::to_vec(&fault_json).unwrap_or_default(),
        ),
        (
            plx_platform::i18n::msg::core_preview_panic(),
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
pub fn event_id_for(build_id: &str, seq: usize, r: &Report) -> String {
    let key = match r {
        Report::Fault(f) => {
            format!(
                "{build_id}|{seq}|sig{}|{:x}|{:x}|{:x}",
                f.signal, f.addr, f.pc, f.lr
            )
        }
        Report::Panic(p) => format!("{build_id}|{seq}|panic|{}|{:x}", p.location, p.message_hash),
        // Not the bucket: it is read from a later line, and an id must not depend on whether that
        // line had been written when a retry read the log.
        Report::Freeze(f) => format!("{build_id}|{seq}|freeze|{}|{:x}|{:x}", f.seq, f.pc, f.lr),
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
    #[cfg(any(test, feature = "test-support"))]
    if let Some(root) = TEST_ROOT.lock().unwrap().as_ref() {
        return root.join("plxnative-crash.log");
    }
    plx_base::paths::in_runtime_dir(plx_base::paths::runtime_file::CRASH)
}

fn mark_paths() -> Vec<std::path::PathBuf> {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(root) = TEST_ROOT.lock().unwrap().as_ref() {
        return vec![root.join("telemetry-crashmark.json")];
    }
    plx_base::paths::telemetry_crashmark_candidates()
}

#[cfg(any(test, feature = "test-support"))]
static TEST_ROOT: std::sync::Mutex<Option<std::path::PathBuf>> = std::sync::Mutex::new(None);

/// Read the crash log's bytes and identity from ONE non-following descriptor
/// (`session::open_owned_regular`). A missing log is a valid empty generation; a symlink, a file
/// somebody else owns, or one larger than `MAX_OWNED_FILE` is an error, and nothing is imported.
fn read_snapshot(path: &std::path::Path) -> std::io::Result<Snapshot> {
    use std::io::Read;
    use std::os::unix::fs::MetadataExt;
    let (file, meta) = match plx_plex::plex::session::open_owned_regular(path) {
        Ok(opened) => opened,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Snapshot::default()),
        Err(e) => return Err(e),
    };
    let max = plx_plex::plex::session::MAX_OWNED_FILE;
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
    let (mut file, meta) = match plx_plex::plex::session::open_owned_regular(path) {
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
    /// Per report: why it is sent from the log, when it is.
    report_path: Vec<Option<NativePath>>,
}

fn reconcile(reports: &[Report], natives: &[Option<super::native::CrashKey>], current_build_id: &str) -> Plan {
    let mut plan = Plan {
        report_native: vec![None; reports.len()],
        native_panic: vec![None; natives.len()],
        report_path: vec![None; reports.len()],
    };
    let mut used = vec![false; natives.len()];
    // What is on disk is all `reconcile` knows about the native path; see [`NativePath`].
    let unsent = if natives.is_empty() { NativePath::NoEnvelope } else { NativePath::EnvelopeUnmatched };
    for (i, r) in reports.iter().enumerate() {
        let (image, signal) = match r {
            Report::Fault(f) => (&f.image, f.signal),
            Report::Panic(p) => (&p.image, SIGABRT),
            // A freeze is not a death: the native daemon wrote nothing for it and there is no
            // envelope to prefer or to drop.
            Report::Freeze(_) => continue,
        };
        let build_id = image.as_ref().map(|x| x.build_id.as_str()).unwrap_or(current_build_id);
        let Some(n) = natives.iter().enumerate().position(|(n, key)| {
            !used[n] && key.as_ref().is_some_and(|k| k.build_id == build_id && k.signal == signal)
        }) else {
            plan.report_path[i] = Some(unsent);
            continue;
        };
        used[n] = true;
        match r {
            Report::Fault(_) => plan.report_native[i] = Some(n),
            Report::Panic(_) => plan.native_panic[n] = Some(i),
            Report::Freeze(_) => {}
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
/// **No debug image for the app's own image without a build id.** An image entry carrying an empty
/// or wrong `debug_id` does not degrade to an unsymbolicated frame with a warning; it produces
/// `missing_symbol` and no error at all, which is indistinguishable from never having uploaded
/// symbols. Sending no image at least says so. The one deliberate exception is a firmware library
/// with no build id: [`module_images`] lists it by name with a nil `debug_id` and no `code_id`,
/// because attributing a crash to a library is worth more than the silent `missing_symbol`.
///
/// **Returns how many crash records this pass FOUND** (local log reports, freezes excluded, plus native envelopes, a
/// crash that left both counts twice): `telemetry::session` reads "more than none" as "the previous
/// process died of a crash", which is what separates a `crashed` release-health session from an
/// `abnormal` one. `0` also when nothing may be read (no consent, no destination).
pub fn recover_pending() -> usize {
    recover_pending_at(&log_path())
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
pub fn may_read_crash_data() -> bool {
    super::consent::allows_errors() && super::sender::has_sentry()
}

fn recover_pending_at(path: &std::path::Path) -> usize {
    if !may_read_crash_data() {
        return 0; // no consent, or nowhere to send — nothing is read and nothing is queued
    }
    let natives = super::native::read_pending();
    // An unreadable log (symlink, foreign owner, over the bound) imports nothing and leaves the mark
    // alone; the native envelopes beside it are still handled.
    let (snapshot, readable) = match read_snapshot(path) {
        Ok(s) => (s, true),
        Err(e) => {
            plx_base::eventlog::log(&format!("telemetry: crash log not imported: {:?}", e.kind()));
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
        native_path: &'a [Option<NativePath>],
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
                Report::Freeze(f) => f.image.as_ref(),
            }
            .map(|x| x.build_id.as_str())
            .unwrap_or(self.current_build_id);
            let did = super::sentry::debug_id(bid);
            let event_id = event_id_for(bid, self.from + i, r);
            let body = match r {
                Report::Fault(f) => {
                    let extra = FaultExtra {
                        native_path: self.native_path[i].unwrap_or(NativePath::NoEnvelope),
                        modules: module_images(f, read_module_head),
                    };
                    sentry_body(f, &event_id, bid, did.as_deref(), self.errors_id.as_deref(), &extra)
                }
                Report::Panic(p) => panic_sentry_body(p, &event_id, self.errors_id.as_deref()),
                Report::Freeze(f) => freeze_sentry_body(
                    f,
                    &event_id,
                    bid,
                    did.as_deref(),
                    self.errors_id.as_deref(),
                    &images_of_mappings(&f.modules, read_module_head),
                ),
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
        native_path: &plan.report_path,
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
        plx_base::eventlog::log(&format!(
            "telemetry: crash log had {} report(s), queued {}, native envelopes {}, native_wins={native_wins}, panic_wins={panic_wins}, symbols={}",
            reports.len(),
            io.queued,
            natives.len(),
            if reports.iter().all(|r| match r {
                Report::Fault(f) => f.image.as_ref().is_some_and(|i| super::sentry::debug_id(&i.build_id).is_some()),
                Report::Panic(_) => true,
                Report::Freeze(f) => f.image.as_ref().is_some_and(|i| super::sentry::debug_id(&i.build_id).is_some()),
            }) { "yes" } else { "partial/no" }
        ));
    }
    crashes_found(&reports, natives.len())
}

/// What `recover_pending` returns and `session::classify_unfinished` reads: how many crash records
/// this pass found. A freeze is a report but not a crash (the process lived on, or was killed by
/// something else), so it does not count.
fn crashes_found(reports: &[Report], natives: usize) -> usize {
    reports.iter().filter(|r| !matches!(r, Report::Freeze(_))).count() + natives
}

/// Make crash consent prospective. When error reporting is switched on, faults already present in
/// the append-only local log belong to the period in which no upload was authorised. Advancing the
/// private watermark before publishing the new consent keeps those local diagnostics local while
/// allowing the next crash to be reported normally. The cutoff is bound to the log it measured
/// (see [`Mark`]) and is not limited by the importer's read bound, so an oversized local log
/// cannot make the opt-in impossible.
pub fn discard_pending_before_opt_in() -> bool {
    match cutoff_mark(&log_path()) {
        Ok(mark) => write_mark(&mark),
        Err(e) => {
            plx_base::eventlog::log(&format!("telemetry: crash log cutoff not taken: {:?}", e.kind()));
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
        plx_base::eventlog::log("telemetry: crash log is not the one the watermark measured — reading it from the start");
        return 0;
    }
    offset
}

fn read_mark() -> Option<Mark> {
    mark_paths()
        .iter()
        .filter_map(|p| plx_plex::plex::session::read_owned_regular(p))
        .find_map(|b| serde_json::from_slice::<Mark>(&b).ok())
}

fn write_mark(mark: &Mark) -> bool {
    let Ok(json) = serde_json::to_vec(mark) else {
        return false;
    };
    let stored = mark_paths()
        .iter()
        .any(|p| plx_plex::plex::session::write_atomic(p, &json).is_ok());
    if !stored {
        // Loud, because the consequence is re-reporting the same crash on every boot until it
        // succeeds — bounded by the deterministic `event_id`, which Sentry dedupes, but still a
        // request per launch that says nothing new.
        plx_base::eventlog::log("telemetry: could not persist the crash watermark to ANY candidate path");
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

    fn extra() -> FaultExtra {
        FaultExtra { native_path: NativePath::NoEnvelope, modules: Vec::new() }
    }

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

    /// **THE GUARANTEE.** No text from the log reaches the event body, apart from the basename of
    /// a library whose mapping passed the path allowlist (see [`ModuleMapping`]: the rest of that
    /// path is kept only to open the file and is never sent).
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
            &extra(),
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
            crash_time: None,
        };
        let id = "0123456789abcdef0123456789abcdef";
        for body in [
            sentry_body(&f, "e", "bid", Some("did"), Some(id), &extra()),
            panic_sentry_body(&p, "e", Some(id)),
        ] {
            let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(v["user"], serde_json::json!({"id": id}));
        }
        for body in [
            sentry_body(&f, "e", "bid", Some("did"), None, &extra()),
            panic_sentry_body(&p, "e", None),
        ] {
            let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert!(
                v.get("user").is_none(),
                "an absent id must leave no user key"
            );
        }
    }

    // ---- fallback events say which modules faulted and why the daemon's report is absent ----

    /// A fault whose `pc` is inside a library and whose `lr` is inside the app's own image.
    const WAYLAND_REC: &str = "\nimg: build_id=11223344556677889900aabbccddeeff00112233 image_addr=0x10000 image_size=0x5f0000\n\
        *** SIGNAL 11 (SIGSEGV) addr=0x0 pc=0xb6f10123 lr=0x4bccd0\n\
        reg: sp=0xbe8f1a90 fp=0x0 ip=0x1 cpsr=0x60000010 r0=0x0 r1=0x2a\n\
        at: b6f00000-b6f21000 r-xp 00000000 fe:01 1234 /usr/lib/libwayland-client.so.0\n\
        at: 00010000-00600000 r-xp 00000000 fe:01 99 /media/developer/x/plxnative\n\
        bin: 00010000-00600000 r-xp 00000000 fe:01 99 /media/developer/x/plxnative\n";

    /// A little-endian ELF32 shared object whose only program header is a `PT_NOTE` holding one
    /// GNU build-id note: all `gnu_build_id` needs, with every offset stated once.
    fn elf_with_build_id(id: &[u8]) -> Vec<u8> {
        let mut note = Vec::new();
        note.extend_from_slice(&4u32.to_le_bytes()); // namesz "GNU\0"
        note.extend_from_slice(&(id.len() as u32).to_le_bytes());
        note.extend_from_slice(&3u32.to_le_bytes()); // NT_GNU_BUILD_ID
        note.extend_from_slice(b"GNU\0");
        note.extend_from_slice(id);
        let off = 52 + 32;
        let mut v = vec![0u8; 52];
        v[..4].copy_from_slice(b"\x7fELF");
        v[4] = 1;
        v[5] = 1;
        v[6] = 1;
        v[16..18].copy_from_slice(&3u16.to_le_bytes()); // ET_DYN
        v[18..20].copy_from_slice(&40u16.to_le_bytes());
        v[28..32].copy_from_slice(&52u32.to_le_bytes()); // e_phoff
        v[40..42].copy_from_slice(&52u16.to_le_bytes());
        v[42..44].copy_from_slice(&32u16.to_le_bytes()); // e_phentsize
        v[44..46].copy_from_slice(&1u16.to_le_bytes()); // e_phnum
        for w in [4u32, off, 0, 0, note.len() as u32, 0, 4, 4] {
            v.extend_from_slice(&w.to_le_bytes());
        }
        v.extend_from_slice(&note);
        v
    }

    fn strings(v: &serde_json::Value, out: &mut Vec<String>) {
        match v {
            serde_json::Value::String(s) => out.push(s.clone()),
            serde_json::Value::Array(a) => a.iter().for_each(|x| strings(x, out)),
            serde_json::Value::Object(o) => o.values().for_each(|x| strings(x, out)),
            _ => {}
        }
    }

    #[test]
    fn fallback_body_carries_hardware_contexts() {
        let body: serde_json::Value = serde_json::from_slice(&sentry_body(
            &only_fault(REC), "e", BUILD, Some("did"), None, &extra(),
        ))
        .unwrap();
        let mut expected = serde_json::json!({});
        super::super::sentry::attach_hardware_context(&mut expected);
        assert_eq!(body["contexts"]["webos"], expected["contexts"]["webos"]);
        assert_eq!(body["contexts"]["hardware"], expected["contexts"]["hardware"]);
        let mut os = serde_json::json!({});
        super::super::sentry::attach_os_context(&mut os);
        assert!(os["contexts"]["os"]["name"].is_string(), "this host has a kernel to name");
        assert_eq!(body["contexts"]["os"], os["contexts"]["os"]);
    }

    /// **A panic report says which kernel, webOS and television it came from, exactly as the fault
    /// fallback does.** The panic body used to carry none of the three contexts, so the same
    /// firmware-specific panic could not be told apart from any other in Sentry's Contexts UI. The
    /// keys are graded against the native sanitiser's own allowlists, not a copy of them.
    #[test]
    fn a_panic_body_carries_the_same_os_webos_and_hardware_contexts_as_a_fault() {
        use super::super::native::{HARDWARE_FIELDS, OS_FIELDS, WEBOS_FIELDS};
        let p = PanicReport {
            location: "src/example.rs:42".into(),
            message_hash: 7,
            image: None,
            crash_time: None,
        };
        let panic: serde_json::Value =
            serde_json::from_slice(&panic_sentry_body(&p, "e", None)).unwrap();
        let fault: serde_json::Value = serde_json::from_slice(&sentry_body(
            &only_fault(REC), "e", BUILD, Some("did"), None, &extra(),
        ))
        .unwrap();
        for key in ["os", "webos", "hardware"] {
            assert!(panic["contexts"][key].is_object(), "panic body lacks contexts.{key}");
            assert_eq!(panic["contexts"][key], fault["contexts"][key], "contexts.{key} differs");
        }
        assert!(panic["contexts"]["hardware"]["probe"].is_string(), "hardware.probe is the why-empty fact");
        // Closed vocabulary only, and nothing but the three contexts (a fault's `crash` context
        // says why the daemon's report is absent, which a panic has no equivalent of).
        let keys_of = |v: &serde_json::Value| -> Vec<String> {
            let mut k: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
            k.sort();
            k
        };
        assert_eq!(keys_of(&panic["contexts"]), ["hardware", "os", "webos"]);
        for (key, allowed) in
            [("os", OS_FIELDS), ("webos", WEBOS_FIELDS), ("hardware", HARDWARE_FIELDS)]
        {
            for field in keys_of(&panic["contexts"][key]) {
                assert!(allowed.contains(&field.as_str()), "contexts.{key}.{field} is not allowlisted");
            }
        }
    }

    #[test]
    fn fallback_body_identifies_pc_and_lr_modules() {
        let f = only_fault(WAYLAND_REC);
        assert_eq!(f.modules.len(), 2, "pc's library and lr's mapping, both from at: lines");
        let id: Vec<u8> = (0u8..20).collect();
        let hex = "000102030405060708090a0b0c0d0e0f10111213";
        let elf = elf_with_build_id(&id);
        let modules = module_images(&f, |m| {
            assert_eq!(m.path,"/usr/lib/libwayland-client.so.0", "the app's own image is read elsewhere");
            Some(elf.clone())
        });
        let body = sentry_body(
            &f, "e", BUILD, super::super::sentry::debug_id(BUILD).as_deref(), None,
            &FaultExtra { native_path: NativePath::NoEnvelope, modules },
        );
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let images = v["debug_meta"]["images"].as_array().unwrap();
        assert_eq!(images.len(), 2, "the app's image and one library");
        assert_eq!(images[0]["code_file"], CODE_FILE);
        let lib = &images[1];
        assert_eq!(lib["code_file"], "libwayland-client.so.0");
        assert_eq!(lib["image_addr"], "0xb6f00000");
        assert_eq!(lib["image_size"], 0x21000);
        assert_eq!(lib["code_id"], hex);
        assert_eq!(lib["debug_id"], super::super::sentry::debug_id(hex).unwrap());
        assert_eq!(v["exception"]["values"][0]["value"], "SIGSEGV at 0x0", "the title groups issues");
        let mut all = Vec::new();
        strings(&v, &mut all);
        let path = all.iter().find(|s| s.starts_with('/'));
        assert!(path.is_none(), "a path reached the body: {path:?}");
        let text = String::from_utf8(body).unwrap();
        assert!(!text.contains("/usr/lib") && !text.contains("/media"));
    }


    // ---- the fallback event carries the crash's own time ----

    fn body_json(f: &Fault) -> serde_json::Value {
        serde_json::from_slice(&sentry_body(f, "e", BUILD, Some("did"), None, &extra())).unwrap()
    }

    #[test]
    fn a_recorded_crash_time_is_parsed_from_its_line() {
        let f = only_fault(&format!("{REC}time: 1750000000\n"));
        assert_eq!(f.crash_time, Some(1_750_000_000));
        assert_eq!(only_fault(REC).crash_time, None, "a record from before the line existed");
        for bad in ["time: \n", "time: -5\n", "time: 17e9\n", "time: 99999999999999999999999\n", "time: 1 2\n"] {
            assert_eq!(only_fault(&format!("{REC}{bad}")).crash_time, None, "{bad:?}");
        }
        // The first stamp wins, so a later stray line cannot move a recorded time.
        let two = only_fault(&format!("{REC}time: 1750000000\ntime: 1750000999\n"));
        assert_eq!(two.crash_time, Some(1_750_000_000));
    }

    #[test]
    fn the_fault_body_carries_the_crash_time_it_was_given() {
        let t = now_secs() - 3_600;
        let f = only_fault(&format!("{REC}time: {t}\n"));
        assert_eq!(body_json(&f)["timestamp"], serde_json::json!(t));
    }

    #[test]
    fn a_fault_without_a_time_gets_no_timestamp_field() {
        let body = body_json(&only_fault(REC));
        assert!(body.get("timestamp").is_none(), "not zero and not now: {body}");
    }

    #[test]
    fn an_absurd_or_future_crash_time_is_omitted_not_sent() {
        let now = now_secs();
        for t in [0, 1, 1_000_000_000, now + 3_600, now + 10 * 365 * 86_400, now - 40 * 86_400] {
            let f = only_fault(&format!("{REC}time: {t}\n"));
            assert!(body_json(&f).get("timestamp").is_none(), "{t} must not be sent");
        }
    }

    #[test]
    fn the_time_window_is_checked_against_now() {
        let now = 1_800_000_000;
        assert_eq!(crash_timestamp(Some(now - 60), now), Some(now - 60));
        assert_eq!(crash_timestamp(Some(now), now), Some(now));
        assert_eq!(crash_timestamp(Some(now + 30), now), Some(now + 30), "a little skew is tolerated");
        assert_eq!(crash_timestamp(Some(now + 3_600), now), None, "future");
        assert_eq!(crash_timestamp(Some(now - 24 * 86_400), now), Some(now - 24 * 86_400));
        assert_eq!(crash_timestamp(Some(now - 26 * 86_400), now), None, "older than Sentry accepts");
        assert_eq!(crash_timestamp(Some(0), now), None);
        assert_eq!(crash_timestamp(None, now), None);
        assert_eq!(crash_timestamp(Some(now), 5), None, "a clock that has not been set is no reference");
    }

    #[test]
    fn a_panic_record_carries_its_crash_time_too() {
        let t = now_secs() - 120;
        let log = format!("*** RUST PANIC [t] at src/a.rs:1: boom\n{}\n", crash_time_record(t));
        let Some(Report::Panic(p)) = parse(&log).into_iter().next() else { panic!("a panic") };
        assert_eq!(p.crash_time, Some(t));
        let v: serde_json::Value = serde_json::from_slice(&panic_sentry_body(&p, "e", None)).unwrap();
        assert_eq!(v["timestamp"], serde_json::json!(t));
        let bare = PanicReport { crash_time: None, ..p };
        let v: serde_json::Value = serde_json::from_slice(&panic_sentry_body(&bare, "e", None)).unwrap();
        assert!(v.get("timestamp").is_none());
    }

    #[test]
    fn a_panics_abort_does_not_move_the_panics_time() {
        let t = now_secs() - 120;
        let log = format!(
            "*** RUST PANIC [t] at src/a.rs:1: boom\n{}\n\n*** SIGNAL 6 (SIGABRT) addr=0x1 pc=0x2 lr=0x3\ntime: {}\n",
            crash_time_record(t),
            t + 1
        );
        let reports = parse(&log);
        assert_eq!(reports.len(), 1);
        let Report::Panic(p) = &reports[0] else { panic!("the panic is the report") };
        assert_eq!(p.crash_time, Some(t));
    }

    // ---- the caller frame is only a caller when lr points into code ----

    fn frame_addrs(v: &serde_json::Value) -> Vec<String> {
        v["exception"]["values"][0]["stacktrace"]["frames"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["instruction_addr"].as_str().unwrap().to_string())
            .collect()
    }

    #[test]
    fn an_lr_that_is_a_stack_address_gets_no_caller_frame() {
        // The device's SIGABRT: lr == sp, a stack address, and no executable mapping holds it.
        let log = "\nimg: build_id=11223344556677889900aabbccddeeff00112233 image_addr=0x10000 image_size=0x5f0000\n\
            *** SIGNAL 6 (SIGABRT) addr=0x4c1b pc=0xb6e1d6a4 lr=0xbe8f1a90\n\
            reg: sp=0xbe8f1a90 fp=0x0 ip=0x1 cpsr=0x60000010\n\
            at: b6e00000-b6e90000 r-xp 00000000 fe:01 77 /lib/libpthread-2.24.so\n\
            at: be8e0000-be901000 rw-p 00000000 00:00 0 [stack]\n";
        let v = body_json(&only_fault(log));
        assert_eq!(frame_addrs(&v), vec!["0xb6e1d6a4"], "only the faulting pc");
        // The register is still reported: it is evidence, just not a caller.
        assert_eq!(v["exception"]["values"][0]["stacktrace"]["registers"]["lr"], "0xbe8f1a90");
    }

    #[test]
    fn an_lr_inside_an_executable_mapping_is_a_caller_frame() {
        // pc in one library, lr in the app's own text (an `at:` line) …
        let v = body_json(&only_fault(WAYLAND_REC));
        assert_eq!(frame_addrs(&v), vec!["0x4bccd0", "0xb6f10123"], "caller first, pc on top");
        // … and lr in another library's executable mapping.
        let log = "*** SIGNAL 11 (SIGSEGV) addr=0x0 pc=0xb6f10123 lr=0xb6e00456\n\
            at: b6f00000-b6f21000 r-xp 00000000 fe:01 1234 /usr/lib/libx.so.0\n\
            at: b6e00000-b6e90000 r-xp 00000000 fe:01 77 /lib/libpthread-2.24.so\n";
        assert_eq!(frame_addrs(&body_json(&only_fault(log))), vec!["0xb6e00456", "0xb6f10123"]);
    }

    #[test]
    fn an_lr_with_no_mapping_in_the_log_gets_no_caller_frame() {
        // No `at:` line for lr, and it is outside the app's own image: nothing vouches for it.
        let log = "*** SIGNAL 11 (SIGSEGV) addr=0x0 pc=0xb6f10123 lr=0xb7777777\n\
            at: b6f00000-b6f21000 r-xp 00000000 fe:01 1234 /usr/lib/libx.so.0\n";
        assert_eq!(frame_addrs(&body_json(&only_fault(log))), vec!["0xb6f10123"]);
    }

    #[test]
    fn an_lr_inside_the_apps_own_image_is_a_caller_even_without_an_at_line() {
        // REC's lr (0x4bccd0) lies in the image the startup marker recorded.
        let v = body_json(&only_fault(REC));
        assert_eq!(frame_addrs(&v), vec!["0x4bccd0", "0x88ef8"]);
    }

    // ---- a module with no GNU build id is still named ----

    /// A little-endian ELF32 with no program headers at all: no note, so no build id.
    fn elf_without_build_id() -> Vec<u8> {
        let mut v = vec![0u8; 52];
        v[..4].copy_from_slice(b"\x7fELF");
        v[4] = 1;
        v[5] = 1;
        v[6] = 1;
        v[16..18].copy_from_slice(&3u16.to_le_bytes());
        v[18..20].copy_from_slice(&40u16.to_le_bytes());
        v[28..32].copy_from_slice(&52u32.to_le_bytes());
        v[40..42].copy_from_slice(&52u16.to_le_bytes());
        v[42..44].copy_from_slice(&32u16.to_le_bytes());
        v
    }

    const ZERO_DEBUG_ID: &str = "00000000-0000-0000-0000-000000000000";

    #[test]
    fn a_module_without_a_build_id_is_named_with_a_zero_debug_id() {
        let f = only_fault(WAYLAND_REC);
        let elf = elf_without_build_id();
        let modules = module_images(&f, |_| Some(elf.clone()));
        assert_eq!(
            modules,
            vec![ModuleImage {
                file: "libwayland-client.so.0".to_string(),
                image_addr: 0xb6f00000,
                image_size: 0x21000,
                code_id: None,
                debug_id: ZERO_DEBUG_ID.to_string(),
            }]
        );
        let body = sentry_body(
            &f, "e", BUILD, super::super::sentry::debug_id(BUILD).as_deref(), None,
            &FaultExtra { native_path: NativePath::NoEnvelope, modules },
        );
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let lib = &v["debug_meta"]["images"][1];
        assert_eq!(lib["code_file"], "libwayland-client.so.0");
        assert_eq!(lib["debug_id"], ZERO_DEBUG_ID);
        assert_eq!(lib["image_addr"], "0xb6f00000");
        assert_eq!(lib["image_size"], 0x21000);
        assert!(lib.get("code_id").is_none(), "no build id to claim: {lib}");
    }

    #[test]
    fn a_module_with_a_build_id_is_unchanged() {
        let f = only_fault(WAYLAND_REC);
        let elf = elf_with_build_id(&(0u8..20).collect::<Vec<_>>());
        let m = module_images(&f, |_| Some(elf.clone()));
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].code_id.as_deref(), Some("000102030405060708090a0b0c0d0e0f10111213"));
        assert_ne!(m[0].debug_id, ZERO_DEBUG_ID);
    }

    #[test]
    fn a_module_that_cannot_be_read_or_is_not_an_elf_is_still_left_out() {
        let f = only_fault(WAYLAND_REC);
        assert!(module_images(&f, |_| None).is_empty(), "dev+inode mismatch or unreadable");
        assert!(module_images(&f, |_| Some(b"not an elf".to_vec())).is_empty());
        assert!(module_images(&f, |_| Some(Vec::new())).is_empty());
    }

    #[test]
    fn a_file_that_is_not_the_one_that_crashed_gets_no_image_even_without_a_build_id() {
        let (dir, same) = installed_module_with("plain-same", 0, &elf_without_build_id());
        let f = Fault { modules: vec![same.clone()], ..Fault::default() };
        let m = module_images(&f, read_module_head);
        assert_eq!(m.len(), 1);
        assert_eq!((m[0].code_id.as_deref(), m[0].debug_id.as_str()), (None, ZERO_DEBUG_ID));
        let (dir2, replaced) = installed_module_with("plain-replaced", 1, &elf_without_build_id());
        let f = Fault { modules: vec![replaced], ..Fault::default() };
        assert!(module_images(&f, read_module_head).is_empty());
        let _ = (std::fs::remove_dir_all(dir), std::fs::remove_dir_all(dir2));
    }

    #[test]
    fn a_path_outside_the_allowlist_is_still_dropped_before_any_read() {
        let log = "*** SIGNAL 11 (SIGSEGV) addr=0x0 pc=0xb6f10123 lr=0x4bccd0\n\
            at: b6f00000-b6f21000 r-xp 00000000 fe:01 1 /usr/lib/lib x.so\n";
        let f = only_fault(log);
        assert!(f.modules.is_empty());
        assert!(module_images(&f, |_| Some(elf_without_build_id())).is_empty());
    }

    #[test]
    fn a_modules_base_is_its_mapping_start_minus_the_file_offset() {
        let log = WAYLAND_REC.replace("b6f00000-b6f21000 r-xp 00000000", "b6f10000-b6f21000 r-xp 00010000");
        let f = only_fault(&log);
        let elf = elf_with_build_id(&[7u8; 20]);
        let m = module_images(&f, |_| Some(elf.clone()));
        assert_eq!(m.len(), 1);
        assert_eq!((m[0].image_addr, m[0].image_size), (0xb6f00000, 0x21000));
    }

    #[test]
    fn only_an_executable_mapping_is_a_module_image() {
        let with = |perms: &str| {
            let log = format!(
                "*** SIGNAL 11 (SIGSEGV) addr=0x0 pc=0xb6f10123 lr=0x4bccd0\n\
                 at: b6f00000-b6f21000 {perms} 00000000 fe:01 1 /usr/lib/libx.so.0\n"
            );
            only_fault(&log).modules.len()
        };
        assert_eq!(with("r-xp"), 1);
        assert_eq!(with("r-xs"), 1);
        assert_eq!(with("rwxp"), 1, "executable is the test, not read-only");
        assert_eq!(with("rw-p"), 0, "a data segment's offset is not its load base");
        assert_eq!(with("r--p"), 0, "relro/rodata");
        assert_eq!(with("---p"), 0, "guard");
    }

    #[test]
    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn st_dev_is_split_the_way_the_kernel_prints_maps() {
        // glibc makedev(0xfe, 0x01), and a minor wider than 8 bits (makedev(8, 0x1_0001)).
        assert_eq!(split_dev(0xfe01), (0xfe, 0x01));
        assert_eq!(split_dev((0x1_0000u64 << 12) | 0x01 | (8 << 8)), (8, 0x1_0001));
    }

    #[test]
    fn only_a_plain_absolute_module_path_that_holds_the_fault_is_kept() {
        let with = |line: &str| {
            let log = format!(
                "*** SIGNAL 11 (SIGSEGV) addr=0x0 pc=0xb6f10123 lr=0x4bccd0\nat: {line}\n"
            );
            only_fault(&log).modules.len()
        };
        assert_eq!(with("b6f00000-b6f21000 r-xp 00000000 fe:01 1 /usr/lib/libx.so.0"), 1);
        assert_eq!(with("b6f00000-b6f21000 r-xp 00000000 fe:01 1 libx.so.0"), 0, "relative");
        assert_eq!(with("b6f00000-b6f21000 r-xp 00000000 fe:01 1 /usr/lib/../x.so"), 0, "dot-dot");
        assert_eq!(with("b6f00000-b6f21000 r-xp 00000000 fe:01 1 /usr/lib/my lib.so"), 0, "space");
        assert_eq!(with("b6f00000-b6f21000 r-xp 00000000 fe:01 1 /usr/lib/x.so (deleted)"), 0);
        assert_eq!(with("b6f00000-b6f21000 rw-p 00000000 00:00 0 [stack]"), 0, "anonymous");
        assert_eq!(with("b6f00000-b6f21000 r-xp 00000000 fe:01 1"), 0, "no path");
        assert_eq!(with("a0000000-a0001000 r-xp 00000000 fe:01 1 /usr/lib/other.so"), 0, "holds neither pc nor lr");
    }

    /// A library file in a scratch directory, and the `at:` line a tracer would have written for
    /// it with the recorded inode shifted by `inode_skew` (0 = the file that is on disk).
    fn installed_module(tag: &str, inode_skew: u64) -> (std::path::PathBuf, ModuleMapping) {
        installed_module_with(tag, inode_skew, &elf_with_build_id(&[9u8; 20]))
    }

    fn installed_module_with(tag: &str, inode_skew: u64, content: &[u8]) -> (std::path::PathBuf, ModuleMapping) {
        use std::os::unix::fs::MetadataExt;
        let dir = std::env::temp_dir().join(format!("plx-crash-module-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("libx.so.0");
        std::fs::write(&path, content).unwrap();
        let meta = std::fs::metadata(&path).unwrap();
        let (major, minor) = split_dev(meta.dev());
        let line = format!(
            "at: b6f00000-b6f21000 r-xp 00000000 {major:x}:{minor:x} {} {}",
            meta.ino() + inode_skew,
            path.display(),
        );
        let m = parse_module(&line).unwrap_or_else(|| panic!("fixture line not accepted: {line}"));
        (dir, m)
    }

    #[test]
    fn a_module_file_that_is_not_the_one_that_crashed_gets_no_image() {
        let (dir, same) = installed_module("same", 0);
        assert!(read_module_head(&same).is_some(), "the file the tracer saw is read");
        let (dir2, replaced) = installed_module("replaced", 1);
        assert!(
            read_module_head(&replaced).is_none(),
            "the mapped inode is not the installed file's: its build id belongs to another build"
        );
        let _ = (std::fs::remove_dir_all(dir), std::fs::remove_dir_all(dir2));
    }

    #[test]
    fn a_module_on_another_device_than_the_one_that_crashed_gets_no_image() {
        let (dir, mut other) = installed_module("device", 0);
        other.dev.1 ^= 0x5a;
        assert!(read_module_head(&other).is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_fifo_in_the_place_of_a_module_is_not_opened_blockingly() {
        let (dir, mut m) = installed_module("fifo", 0);
        let fifo = dir.join("pipe");
        let c = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        m.path = fifo.to_str().unwrap().to_string();
        assert!(read_module_head(&m).is_none(), "returns at all, and with nothing");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn dev_and_inode_are_parsed_from_the_maps_line() {
        let m = parse_module("at: b6f00000-b6f21000 r-xp 00000000 fe:0a 1234 /usr/lib/libx.so.0").unwrap();
        assert_eq!((m.dev, m.inode), ((0xfe, 0x0a), 1234));
        for bad in ["fe0a 1234", "fe:0a x", "zz:0a 1", "fe: 1"] {
            let line = format!("at: b6f00000-b6f21000 r-xp 00000000 {bad} /usr/lib/libx.so.0");
            assert!(parse_module(&line).is_none(), "{bad}");
        }
    }

    #[test]
    fn fallback_body_names_native_path() {
        let fault = parse(REC).remove(0);
        // Nothing pending at all.
        let none = reconcile(&[fault.clone()], &[], BUILD);
        assert_eq!(none.report_path, vec![Some(NativePath::NoEnvelope)]);
        // Something pending, but not this crash's signal.
        let other = reconcile(&[fault.clone()], &[key(6)], BUILD);
        assert_eq!(other.report_path, vec![Some(NativePath::EnvelopeUnmatched)]);
        // The native envelope won: the report is not sent, so it has no path to name.
        let won = reconcile(&[fault.clone()], &[key(11)], BUILD);
        assert_eq!(won.report_path, vec![None]);
        // …and the body says it, in the closed vocabulary.
        let Report::Fault(f) = &fault else { unreachable!() };
        for (path, name) in [
            (NativePath::NoEnvelope, "no_envelope"),
            (NativePath::EnvelopeUnmatched, "envelope_unmatched"),
        ] {
            let body: serde_json::Value = serde_json::from_slice(&sentry_body(
                f, "e", BUILD, Some("did"), None, &FaultExtra { native_path: path, modules: Vec::new() },
            ))
            .unwrap();
            assert_eq!(body["contexts"]["crash"]["native_path"], name);
        }
    }

    #[test]
    fn the_fault_preview_declares_the_new_fields_as_placeholders() {
        let preview = preview_events();
        let fault: serde_json::Value = serde_json::from_slice(&preview[0].1).unwrap();
        assert_eq!(fault["contexts"]["os"]["version"], "<kernel release>", "never this host's kernel");
        assert_eq!(
            fault["contexts"]["hardware"]["probe"],
            "<ok / no_keys / unreadable / unprobed per nyx file>",
            "the fallback report carries `probe` even though the native crash event does not"
        );
        // A closed value, shown as a real one like the signal name: PRIVACY.md lists both.
        assert_eq!(fault["contexts"]["crash"]["native_path"], "no_envelope");
        let images = fault["debug_meta"]["images"].as_array().unwrap();
        assert_eq!(images.len(), 2, "the app's image and a library");
        assert_eq!(images[1]["code_file"], "<library file name>");
        let mut all = Vec::new();
        strings(&fault, &mut all);
        assert!(all.iter().all(|s| !s.starts_with('/')));
    }

    /// The panic preview shows the same three contexts as placeholders, never this host's kernel or
    /// model, so the person deciding what to share sees what a panic report carries.
    #[test]
    fn the_panic_preview_declares_the_contexts_as_placeholders() {
        let preview = preview_events();
        let fault: serde_json::Value = serde_json::from_slice(&preview[0].1).unwrap();
        let panic: serde_json::Value = serde_json::from_slice(&preview[1].1).unwrap();
        for key in ["os", "webos", "hardware"] {
            assert_eq!(panic["contexts"][key], fault["contexts"][key], "contexts.{key}");
        }
        assert_eq!(panic["contexts"]["os"]["version"], "<kernel release>");
        assert_eq!(panic["contexts"]["hardware"]["probe"], FALLBACK_PROBE_PLACEHOLDER);
        assert!(panic["contexts"].get("crash").is_none(), "a panic has no native_path");
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
            &extra(),
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
            super::super::release()
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
            String::from_utf8(sentry_body(&only_fault(REC), "e", "b", Some("d"), None, &extra())).unwrap();
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
            crash_time: None,
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
        let _g = plx_base::testlock::serial();
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
        let _g = plx_base::testlock::serial();
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
        let _g = plx_base::testlock::serial();
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
            _ => panic!("expected fault"),
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
        let with = String::from_utf8(sentry_body(&f, "e", "bid", Some("did"), None, &extra())).unwrap();
        assert!(with.contains("debug_meta") && with.contains("\"debug_id\":\"did\""));

        let without = String::from_utf8(sentry_body(&f, "e", "bid", None, None, &extra())).unwrap();
        assert!(
            !without.contains("debug_meta"),
            "an image was claimed with no id to pair it"
        );
        // The addresses still travel: an unsymbolicated report is worth far more than none.
        assert!(without.contains("0x88ef8") && without.contains("0x4bccd0"));

        let no_identity = Fault { image: None, ..f };
        let no_build =
            String::from_utf8(sentry_body(&no_identity, "e", "", Some("did"), None, &extra())).unwrap();
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
        let _g = plx_base::testlock::serial();
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
    // ---- main-thread freezes ----

    /// A real freeze record in the exact shape `freeze_handler` and the watchdog write: the
    /// startup marker, the header, the time, the registers, the walk, the mappings and the
    /// recovery line.
    const FRZ: &str = "\nimg: build_id=11223344556677889900aabbccddeeff00112233 image_addr=0x10000 image_size=0x5f0000\n\
                       \n*** FREEZE seq=1 pc=0xb6f10000 lr=0x4bccd0\n\
                       time: 1750000000\n\
                       reg: sp=0xbe8f1a90 fp=0xbe8f1aa0 ip=0x1 cpsr=0x60000010 r0=0x0 r1=0x2a\n\
                       frames: 0x4bccd0 0x4a1234 0xb6f01000\n\
                       at: b6f00000-b6f21000 r-xp 00000000 fe:01 1234 /lib/libc.so.6\n\
                       bin: 00010000-00600000 r-xp 00000000 fe:01 99 /media/developer/x/plxnative\n\
                       freeze-end: seq=1 bucket=10-30s\n";

    fn only_freeze(log: &str) -> Freeze {
        match parse(log).into_iter().next().expect("a report") {
            Report::Freeze(f) => f,
            other => panic!("expected a freeze, got {other:?}"),
        }
    }

    fn freeze_json(f: &Freeze) -> serde_json::Value {
        serde_json::from_slice(&freeze_sentry_body(f, "e", BUILD, Some("did"), None, &[])).unwrap()
    }

    #[test]
    fn a_freeze_record_is_parsed_with_its_walk_its_mappings_and_its_recovery() {
        let f = only_freeze(FRZ);
        assert_eq!((f.seq, f.pc, f.lr), (1, 0xb6f1_0000, 0x4b_ccd0));
        assert_eq!(f.frames, vec![0x4b_ccd0, 0x4a_1234, 0xb6f0_1000]);
        assert_eq!(f.bucket, FreezeBucket::Medium);
        assert_eq!(f.crash_time, Some(1_750_000_000));
        assert_eq!(f.registers.r1, Some(0x2a));
        assert_eq!(f.registers.pc, Some(0xb6f1_0000), "the header's pc is the register file's pc");
        assert_eq!(f.image.as_ref().map(|i| i.image_addr), Some(0x10000));
        // libc holds pc and a frame; the app's own `bin:` line is not an `at:` line and is ignored.
        assert_eq!(f.modules.len(), 1);
        assert_eq!(f.modules[0].file_name(), "libc.so.6");
        assert_eq!(parse(FRZ).len(), 1, "a freeze is one report and its recovery line is not another");
    }

    #[test]
    fn a_freeze_with_no_recovery_line_is_unrecovered() {
        let log = FRZ.replace("freeze-end: seq=1 bucket=10-30s\n", "");
        let f = only_freeze(&log);
        assert_eq!(f.bucket, FreezeBucket::Unrecovered);
        let body = freeze_json(&f);
        assert_eq!(body["tags"]["freeze"], "unrecovered");
        assert_eq!(body["exception"]["values"][0]["mechanism"]["handled"], false);
    }

    #[test]
    fn a_recovery_line_closes_only_a_freeze_of_its_own_process() {
        // Process A froze (probe 1) and was killed still stuck; process B's own probe 1 recovered.
        let a = FRZ.replace("freeze-end: seq=1 bucket=10-30s\n", "");
        let b_marker = "\nimg: build_id=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa image_addr=0x10000 image_size=0x5f0000\n";
        let log = format!("{a}{b_marker}freeze-end: seq=1 bucket=5-10s\n");
        let reports = parse(&log);
        assert_eq!(reports.len(), 1);
        let Report::Freeze(f) = &reports[0] else { panic!("a freeze") };
        assert_eq!(f.bucket, FreezeBucket::Unrecovered, "the later process's recovery is not this freeze's");
    }

    #[test]
    fn a_recovery_pairs_by_probe_number_when_a_crash_record_sits_between() {
        let log = format!(
            "{}\n*** SIGNAL 11 (SIGSEGV) addr=0x0 pc=0x88ef8 lr=0x4bccd0\nfreeze-end: seq=1 bucket=30s+\n",
            FRZ.replace("freeze-end: seq=1 bucket=10-30s\n", "")
        );
        let reports = parse(&log);
        assert_eq!(reports.len(), 2);
        assert!(matches!(&reports[0], Report::Freeze(f) if f.bucket == FreezeBucket::Long));
        assert!(matches!(reports[1], Report::Fault(_)));
    }

    #[test]
    fn a_reg_line_attaches_only_to_the_record_it_follows() {
        // A freeze header, then a crash header, then ONE `reg:` line: the crash's own, written
        // right after its header. The freeze must not take it, and the crash must.
        let log = "\n*** FREEZE seq=1 pc=0xb6f10000 lr=0x4bccd0\n\
                   *** SIGNAL 11 (SIGSEGV) addr=0x0 pc=0x88ef8 lr=0x4bccd0\n\
                   reg: sp=0xbe8f1a90 fp=0x0 ip=0x1 cpsr=0x60000010 r0=0x0 r1=0x2a\n";
        let reports = parse(log);
        let Some(Report::Fault(fault)) = reports.last() else { panic!("a fault: {reports:?}") };
        assert_eq!(fault.registers.r1, Some(0x2a), "the crash keeps the register line that followed it");
        // A `reg:` after a skipped header attaches to nothing.
        let log = "\n*** FREEZE seq=1 pc=0xb6f10000 lr=0x4bccd0\n*** FREEZE seq=bad pc=0x1 lr=0x1\nreg: sp=0x1 r1=0x2a\n";
        let Report::Freeze(f) = &parse(log)[0] else { panic!("a freeze") };
        assert_eq!(f.registers.r1, None, "a register line that follows another record's header is not this freeze's");
    }

    #[test]
    fn an_unrecovered_freeze_followed_by_a_crash_of_its_process_is_dropped() {
        let unrecovered = FRZ.replace("freeze-end: seq=1 bucket=10-30s\n", "");
        let crash = "\n*** SIGNAL 11 (SIGSEGV) addr=0x0 pc=0x88ef8 lr=0x4bccd0\n";
        let reports = parse(&format!("{unrecovered}{crash}"));
        assert_eq!(reports.len(), 1, "the crash is the report: {reports:?}");
        assert!(matches!(reports[0], Report::Fault(_)));
        let panic = "\n*** RUST PANIC [t] at src/a.rs:1: boom\n";
        let reports = parse(&format!("{unrecovered}{panic}"));
        assert!(reports.iter().any(|r| matches!(r, Report::Panic(_))), "the panic is parsed: {reports:?}");
        assert!(reports.iter().all(|r| !matches!(r, Report::Freeze(_))), "{reports:?}");
        // A recovered freeze is kept, and so is one from an EARLIER process than the crash.
        assert_eq!(parse(&format!("{FRZ}{crash}")).len(), 2);
        let next_process = "\nimg: build_id=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa image_addr=0x10000 image_size=0x5f0000\n";
        let reports = parse(&format!("{unrecovered}{next_process}{crash}"));
        assert_eq!(reports.len(), 2, "the earlier process's stuck freeze stays: {reports:?}");
    }

    #[test]
    fn freeze_lines_are_read_under_bounds() {
        // A walk longer than the tracer's cap is cut at the cap, across lines too.
        let many = (0..40).map(|i| format!("0x{:x}", 0x1000 + i)).collect::<Vec<_>>().join(" ");
        let f = only_freeze(&format!("{FRZ}frames: {many}\nframes: {many}\n"));
        assert_eq!(f.frames.len(), MAX_FREEZE_FRAMES);

        // One token that is not a short hex number refuses the whole line.
        for bad in ["frames: 0x1000 zzz\n", "frames: 0x1000 0x123456789\n", "frames: 0x\n", "frames: 4096\n"] {
            let log = FRZ.replace("frames: 0x4bccd0 0x4a1234 0xb6f01000\n", bad);
            assert!(only_freeze(&log).frames.is_empty(), "{bad:?}");
        }

        // A bucket the table does not spell, a raw duration, trailing junk and a bad number are
        // not recoveries.
        for bad in [
            "freeze-end: seq=1 bucket=12345ms\n",
            "freeze-end: seq=1 bucket=unrecovered\n",
            "freeze-end: seq=1 bucket=10-30s extra\n",
            "freeze-end: seq=x bucket=10-30s\n",
            "freeze-end: seq=1\n",
        ] {
            let log = FRZ.replace("freeze-end: seq=1 bucket=10-30s\n", bad);
            assert_eq!(only_freeze(&log).bucket, FreezeBucket::Unrecovered, "{bad:?}");
        }

        // A header without its three numbers is no record, and its follow-on lines attach to nothing.
        for bad in ["*** FREEZE seq=1 pc=0x10\n", "*** FREEZE pc=0x10 lr=0x10\n", "*** FREEZE seq=-1 pc=0x1 lr=0x1\n", "*** FREEZE seq=99999999999 pc=0x1 lr=0x1\n"] {
            assert!(parse(&format!("{bad}frames: 0x1\nreg: sp=0x1\n")).is_empty(), "{bad:?}");
        }
    }

    #[test]
    fn a_freeze_body_has_the_agreed_shape() {
        let f = only_freeze(FRZ);
        let body = freeze_json(&f);
        assert_eq!(body["level"], "error");
        assert_eq!(body["tags"]["freeze"], "10-30s");
        let ex = &body["exception"]["values"][0];
        assert_eq!(ex["type"], "MainThreadFreeze");
        assert_eq!(ex["value"], "Main thread frozen for 10-30s");
        assert_eq!(ex["mechanism"]["type"], "freeze");
        assert_eq!(ex["mechanism"]["handled"], true);
        assert_eq!(body["dist"], "11223344556677889900aabbccddeeff00112233");
        // Oldest first: the outermost walked frame leads, pc is last; lr is not repeated by the
        // walk's own first entry.
        let frames = ex["stacktrace"]["frames"].as_array().unwrap();
        let addrs: Vec<_> = frames.iter().map(|x| x["instruction_addr"].as_str().unwrap()).collect();
        assert_eq!(addrs, ["0xb6f01000", "0x4a1234", "0x4bccd0", "0xb6f10000"]);
        assert_eq!(frames[3]["trust"], "context");
        assert_eq!(frames[0]["trust"], "fp");
        assert_eq!(ex["stacktrace"]["registers"]["r1"], "0x2a");
        assert!(body["timestamp"].is_u64() || body.get("timestamp").is_none(), "the time is the record's, window permitting");
    }

    #[test]
    fn a_freeze_fingerprints_on_the_nearest_app_frame_and_never_on_the_duration() {
        let f = only_freeze(FRZ);
        // pc is parked in libc (offset 0x10000); the first frame in the app's own image names the
        // call that waited: 0x4bccd0 less the image base 0x10000.
        assert_eq!(freeze_json(&f)["fingerprint"], serde_json::json!(["freeze", "plxnative+0x4accd0"]));

        // The same stall at another duration is the same issue.
        let long = Freeze { bucket: FreezeBucket::Long, ..f.clone() };
        assert_eq!(freeze_json(&long)["fingerprint"], freeze_json(&f)["fingerprint"]);

        // No image identity and no app frame: the nearest located frame, as library + offset.
        let no_image = Freeze { image: None, frames: Vec::new(), lr: 0, ..f.clone() };
        assert_eq!(freeze_json(&no_image)["fingerprint"], serde_json::json!(["freeze", "libc.so.6+0x10000"]));

        // Nothing located: a constant word, never a raw (per-boot) address.
        let nothing = Freeze { image: None, modules: Vec::new(), ..f };
        assert_eq!(freeze_json(&nothing)["fingerprint"], serde_json::json!(["freeze", "unmapped"]));
    }

    #[test]
    fn no_text_from_a_freeze_record_reaches_the_wire() {
        let log = FRZ
            .replace("*** FREEZE seq=1 pc=0xb6f10000 lr=0x4bccd0", "*** FREEZE seq=1 pc=0xb6f10000 lr=0x4bccd0 TITLE=SecretMovie")
            .replace("time: 1750000000", "time: 1750000000 SecretMovie")
            .replace("freeze-end: seq=1 bucket=10-30s", "freeze-end: seq=1 bucket=10-30s SecretMovie");
        let f = only_freeze(&log);
        let text = String::from_utf8(freeze_sentry_body(&f, "e", BUILD, Some("did"), None, &[])).unwrap();
        assert!(!text.contains("SecretMovie"));
        assert!(!text.contains("/lib/") && !text.contains("/media"), "only a library's basename may travel");
    }

    #[test]
    fn a_freeze_event_id_is_stable_across_retries_and_ignores_the_bucket() {
        let f = only_freeze(FRZ);
        let r = Report::Freeze(f.clone());
        let id = event_id_for(BUILD, 7, &r);
        assert_eq!(id, event_id_for(BUILD, 7, &r));
        assert_ne!(id, event_id_for(BUILD, 8, &r), "its position in the log is part of the key");
        let unrecovered = Report::Freeze(Freeze { bucket: FreezeBucket::Unrecovered, ..f });
        assert_eq!(id, event_id_for(BUILD, 7, &unrecovered));
    }


    /// A freeze is recovered (or reported unrecovered) after the fact, never a crash: it must not
    /// make the previous release-health session `crashed`.
    #[test]
    fn a_freeze_only_log_finds_no_crash_so_its_session_is_abnormal() {
        use super::super::session::{classify_unfinished, Status, Stored};
        let unrecovered = FRZ.replace("freeze-end: seq=1 bucket=10-30s\n", "");
        for log in [FRZ.to_string(), unrecovered] {
            let reports = parse(&log);
            assert!(matches!(reports.as_slice(), [Report::Freeze(_)]), "{reports:?}");
            let found = crashes_found(&reports, 0);
            assert_eq!(found, 0, "a freeze is not a crash");
            let stored = Stored {
                sid: "11111111-1111-4111-8111-111111111111".into(),
                did: "d".into(),
                release: "plxnative@1.2.3".into(),
                environment: "production".into(),
                started: 1_800_000_000,
                last_active: 1_800_000_100,
            };
            let f = classify_unfinished(stored, found, 1_800_100_000, "d").expect("reported");
            assert_eq!(f.status, Status::Abnormal);
        }
        let crash = parse("\n*** SIGNAL 11 (SIGSEGV) addr=0x0 pc=0x88ef8 lr=0x4bccd0\n");
        assert_eq!(crashes_found(&crash, 0), 1);
        assert_eq!(crashes_found(&[], 1), 1, "a native envelope alone is a crash");
        assert_eq!(crashes_found(&parse(FRZ), 1), 1);
    }
}
