//! The recorder and the recording (restructure spec §5.3–§5.5): what a controlled boot writes,
//! how it is bounded, and how it is read back for replay.
//!
//! **Format.** A directory: `manifest.json` (the header) and `rec-NNNN.jsonl` segments, one JSON
//! object per line, `{"f":<frame>,"t":"<kind>",…}`. Kinds per frame: `tick` (EVERY frame),
//! `present` (the bit and WHY), `in` (an input, with its recorded resolution), `eff` (an effect,
//! `from`/`e`/`addr`), `async` (an adapter result's address and payload or blob hash), `land`
//! (schema 2: how many landings a STORE consumed on this frame — the schedule `ui::landgate`
//! holds a replay to), `life`, `timer`, `st` (the logical-state hash, on every EVENT frame). The
//! header carries `schema`,
//! `state_fp`, the build, the features, the armed triggers, `init` (the application's initial
//! conditions, `H::Init`) and the clock origin.
//!
//! **Bounds.** Buffered — one write per frame at most; segment rotation at 2 MiB; a HARD byte cap
//! (64 MB: the runtime root is a RAM-backed tmpfs on the set) after which the recording STOPS and
//! says so; directory 0700, files 0600. No `scrub_local` on this path: the directory is private
//! (`.gitignore`, `outbound-guard.py`'s `PRIVATE_DIRS`).
//! Replay additionally accounts for source plus decoded structures within that reservation,
//! charging JSON nodes/strings and frame capacity while parsing, before collecting more data.
//! Every storage error terminally latches; finish returns final-flush failure to the app.
//!
//! **Arming.** A recording starts ONLY at a controlled boot: `Writer::open` takes the frame index
//! and refuses anything but 0, so the header is a complete initial condition by construction —
//! empty queues, no timers, nothing in flight. A mid-session checkpoint is not promised.
//!
//! **Replay** is `ui::replay`. This module knows nothing about machines; it stores lines.
#![allow(dead_code)] // phase 2: the product loop's tap lands with the recorder trigger

use std::collections::HashMap;
use std::ffi::CStr;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::machine::{Canon, LogicalState, Measure, Tick};
mod strict_json;

/// The record format's version. A recording from another schema is REFUSED, both printed.
///
/// **2 (phase 11): `land`.** A recording now carries the frame every STORE consumed a landing on
/// — the schedule `ui::landgate` holds a replay's live landings to (§3.3 step 3). Schema 1 could
/// not: the only per-frame arrival it recorded was the dispatcher's `async`, so every result the
/// legacy pumps drain OUTSIDE that drain had no recorded frame at all, and a schema-1 recording
/// replayed under the gate would silently grade nothing. Refusing it is the honest answer;
/// `tools/plxnative-rec rerecord` is the verb (`tests/fixtures/replay/README.md`).
pub const SCHEMA: u32 = 2;
/// Segment rotation.
pub const SEGMENT_BYTES: usize = 2 * 1024 * 1024;
/// The hard cap on one recording (spec §5.3, settled on the tmpfs measurement).
pub const CAP_BYTES: usize = 64 * 1024 * 1024;

/// Only this application's runtime namespace, never the target named by recplay or a header.
/// Caller must first consume the active Writer. remove_dir_all does not follow directory
/// symlinks; the explicit symlink branch also handles dangling links as owned entries.
pub(crate) fn erase_owned_artifacts(root: &Path) -> Vec<String> {
    let mut failures = Vec::new();
    for name in ["plxnative-rec", "plxnative-recplay", "plxnative-app-init", "plxnative-recordings"] {
        let path = root.join(name);
        let result = match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
            Ok(metadata) if name == "plxnative-recordings" && metadata.is_dir() => fs::remove_dir_all(&path),
            Ok(_) => fs::remove_file(&path),
        };
        if result.is_err() { failures.push(format!("{name}: could not remove owned recording artifact")); }
    }
    failures
}
// Enough for the final cap record with full-width frame/byte counters. The manifest and normal
// segments share the remaining budget; stopping must not itself exceed the hard cap.
const CAP_NOTE_RESERVE: usize = 128;
const DATA_CAP_BYTES: usize = CAP_BYTES - CAP_NOTE_RESERVE;

/// What a recording is refused for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RecError {
    /// `Writer::open` was asked to start anywhere but frame 0.
    MidSession { at_frame: u64 },
    InitialTooLarge { limit: usize },
    RecordingTooLarge { limit: usize },
    Io(String),
    /// The loader met another schema: `(theirs, ours)`.
    Schema { theirs: u32, ours: u32 },
    /// The loader met another state shape: `(theirs, ours)`.
    StateShape { theirs: u64, ours: u64 },
    Malformed { line: usize, what: String },
}

/// The header (spec §5.3).
#[derive(Clone, Debug)]
pub struct Header {
    pub schema: u32,
    pub state_fp: u64,
    pub build: String,
    pub features: Vec<String>,
    pub triggers: Vec<String>,
    /// The application's initial conditions, as its `LogicalState::probe` text and hash.
    pub init_probe: String,
    pub init_hash: u64,
    /// Application-defined initial contents. The library transports them without interpreting
    /// application state; the host's state-shape fingerprint versions this payload.
    pub init_data: Value,
    pub clock_start_ms: u32,
    /// `true` when blob capture was opted into (`plxnative-rec=blobs`).
    pub blobs: bool,
}

#[derive(serde::Serialize)]
struct HeaderWire<'a> {
    schema: u32,
    state_fp: u64,
    build: &'a str,
    features: &'a [String],
    triggers: &'a [String],
    init: InitWire<'a>,
    clock: ClockWire,
    blobs: bool,
}
#[derive(serde::Serialize)]
struct InitWire<'a> { probe: &'a str, hash: u64, data: &'a Value }
#[derive(serde::Serialize)]
struct ClockWire { start: u32 }

/// Serialize without first cloning the complete initial Value into another JSON tree, and stop
/// allocating output at the data budget. No manifest/segment is opened until encoding succeeds.
struct ManifestBuffer { bytes: Vec<u8>, exceeded: bool }
impl Write for ManifestBuffer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > DATA_CAP_BYTES.saturating_sub(self.bytes.len()) {
            self.exceeded = true;
            return Err(std::io::Error::other("initial contents exceed recording cap"));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
}

impl Header {
    pub fn new(state_fp: u64, init: &dyn LogicalState) -> Self {
        let mut probe = String::new();
        init.probe(&mut probe);
        Self {
            schema: SCHEMA,
            state_fp,
            build: String::new(),
            features: Vec::new(),
            triggers: Vec::new(),
            init_probe: probe,
            init_hash: init.hash(),
            init_data: Value::Null,
            clock_start_ms: 0,
            blobs: false,
        }
    }

    fn wire(&self) -> HeaderWire<'_> {
        let Self { schema, state_fp, build, features, triggers, init_probe, init_hash, init_data,
            clock_start_ms, blobs } = self;
        HeaderWire { schema: *schema, state_fp: *state_fp, build, features, triggers,
            init: InitWire { probe: init_probe, hash: *init_hash, data: init_data },
            clock: ClockWire { start: *clock_start_ms }, blobs: *blobs }
    }

    #[cfg(test)]
    pub(crate) fn to_json(&self) -> Value {
        serde_json::to_value(self.wire()).expect("header wire contains only JSON values")
    }

    fn from_json(v: &Value) -> Result<Self, RecError> {
        if !v.is_object() || v.get("init").is_some_and(|init| !init.is_object()) {
            return Err(malformed(0,"header object"));
        }
        let get_u64 = |k: &str| v.get(k).and_then(Value::as_u64);
        let text = |value: Option<&Value>| -> Result<String,RecError> {
            value.map_or(Ok(String::new()),|value| value.as_str().map(String::from).ok_or_else(|| malformed(0,"header text")))
        };
        Ok(Self {
            schema: checked_u32(v,"schema",0)?,
            state_fp: get_u64("state_fp").ok_or_else(|| malformed(0, "state_fp"))?,
            build: text(v.get("build"))?,
            features: strings(v.get("features"))?,
            triggers: strings(v.get("triggers"))?,
            init_probe: text(v["init"].get("probe"))?,
            init_hash: v["init"].get("hash").map_or(Ok(0),|value| value.as_u64().ok_or_else(|| malformed(0,"initial hash")))?,
            init_data: v["init"].get("data").cloned().unwrap_or(Value::Null),
            clock_start_ms: if v.get("clock").is_some() { checked_u32(&v["clock"],"start",0)? } else { 0 },
            blobs: v.get("blobs").map_or(Ok(false),|value| value.as_bool().ok_or_else(|| malformed(0,"blob policy")))?,
        })
    }
}

fn strings(v: Option<&Value>) -> Result<Vec<String>, RecError> {
    let Some(v) = v else { return Ok(Vec::new()) };
    v.as_array().ok_or_else(|| malformed(0,"string array"))?.iter()
        .map(|x| x.as_str().map(String::from).ok_or_else(|| malformed(0,"string array element"))).collect()
}

fn malformed(line: usize, what: &str) -> RecError {
    RecError::Malformed {
        line,
        what: what.to_string(),
    }
}

fn checked_u32(v: &Value, field: &str, line: usize) -> Result<u32, RecError> {
    v[field].as_u64().and_then(|n| u32::try_from(n).ok()).ok_or_else(|| malformed(line,field))
}

fn input_budget(manifest: usize, segments: impl IntoIterator<Item = usize>) -> Result<(), RecError> {
    let mut total = manifest;
    for size in segments {
        total = total.checked_add(size).ok_or(RecError::RecordingTooLarge { limit:CAP_BYTES })?;
    }
    if total > CAP_BYTES { return Err(RecError::RecordingTooLarge { limit:CAP_BYTES }); }
    Ok(())
}

fn bounded_read(path: &Path, remaining: usize) -> Result<Vec<u8>, RecError> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path).map_err(|_| RecError::Io("cannot open recording file".into()))?;
    let metadata = file.metadata().map_err(|_| RecError::Io("cannot inspect recording file".into()))?;
    if !metadata.is_file() { return Err(RecError::Io("recording input is not a regular file".into())); }
    if metadata.len() > remaining as u64 { return Err(RecError::RecordingTooLarge { limit:CAP_BYTES }); }
    // Exact allocation avoids read_to_end's geometric spare capacity and a limit+1 growth
    // doubling. A changing file is refused, never allowed to grow this preflight allocation.
    let mut bytes = vec![0; metadata.len() as usize];
    file.read_exact(&mut bytes).map_err(|_| RecError::Io("cannot read recording file".into()))?;
    let mut extra = [0];
    if file.read(&mut extra).map_err(|_| RecError::Io("cannot finish recording read".into()))? != 0 {
        return Err(RecError::Io("recording changed during read".into()));
    }
    Ok(bytes)
}

/// Generic typed-initial input transport, with exactly the recording reader's bounds and JSON
/// integrity checks. Application decoding happens afterwards and never reads a fallback file.
pub(crate) fn initial_value(path: &Path) -> Result<Value, RecError> {
    strict_json::decode(&bounded_read(path,DATA_CAP_BYTES)?).map_err(|_| malformed(0,"initial JSON"))
}

pub(crate) fn mode_value(path: &Path) -> Result<Option<String>, RecError> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(RecError::Io("cannot inspect recorder trigger".into())),
        Ok(_) => {}
    }
    let bytes = bounded_read(path,libc::PATH_MAX as usize)?;
    let value = String::from_utf8(bytes).map_err(|_| malformed(0,"recorder trigger UTF-8"))?;
    Ok(Some(value.trim().to_owned()))
}

/// Where the bytes go: a directory of segments (the product), or memory (tests).
pub trait Sink {
    fn segment(&mut self, index: u32) -> std::io::Result<Box<dyn Write>>;
    fn manifest(&mut self, text: &str) -> std::io::Result<()>;
    /// Abort an unattached/failed-start capture. Unsupported sinks fail closed, never claim
    /// that persistent data was removed. No pathname from a recording authorizes rollback.
    fn rollback(&mut self) -> std::io::Result<()> {
        Err(std::io::Error::other("sink cannot roll back a capture"))
    }
}

/// The product sink: `dir/manifest.json`, `dir/rec-NNNN.jsonl`, private modes.
pub struct DirSink {
    dir: PathBuf,
    directory_identity: (u64,u64),
    created: Vec<(PathBuf,CreatedIdentity)>,
    #[cfg(test)]
    fail_initial_metadata: bool,
}

enum CreatedIdentity {
    /// Registered immediately after create_new, before the first fallible metadata query.
    Open(fs::File),
    Known((u64,u64)),
}

fn file_identity(metadata: &fs::Metadata) -> (u64,u64) {
    use std::os::unix::fs::MetadataExt;
    (metadata.dev(),metadata.ino())
}

impl DirSink {
    pub fn create(dir: &Path) -> std::io::Result<Self> {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
        if !fs::symlink_metadata(dir)?.is_dir() {
            return Err(std::io::Error::other("recording directory must not be a symlink"));
        }
        private_mode(dir, 0o700)?;
        Ok(Self {
            dir: dir.to_path_buf(),
            directory_identity: file_identity(&fs::symlink_metadata(dir)?),
            created: Vec::new(),
            #[cfg(test)] fail_initial_metadata: false,
        })
    }
    fn create_owned_file(&mut self, path: &Path) -> std::io::Result<fs::File> {
        let file = private_new_file(path)?;
        self.created.push((path.to_path_buf(),CreatedIdentity::Open(file)));
        #[cfg(test)]
        if std::mem::take(&mut self.fail_initial_metadata) {
            return Err(std::io::Error::other("injected initial metadata failure"));
        }
        let claim = &mut self.created.last_mut().expect("just registered file").1;
        let CreatedIdentity::Open(file) = claim else { unreachable!() };
        let identity = file_identity(&file.metadata()?);
        let CreatedIdentity::Open(file) = std::mem::replace(claim,CreatedIdentity::Known(identity)) else { unreachable!() };
        Ok(file)
    }
}

#[cfg(unix)]
fn private_mode(p: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(p, fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn private_mode(_p: &Path, _mode: u32) -> std::io::Result<()> {
    Ok(())
}

impl Sink for DirSink {
    fn segment(&mut self, index: u32) -> std::io::Result<Box<dyn Write>> {
        let p = self.dir.join(format!("rec-{index:04}.jsonl"));
        let f = self.create_owned_file(&p)?;
        Ok(Box::new(std::io::BufWriter::new(f)))
    }

    fn manifest(&mut self, text: &str) -> std::io::Result<()> {
        let p = self.dir.join("manifest.json");
        self.create_owned_file(&p)?.write_all(text.as_bytes())
    }

    fn rollback(&mut self) -> std::io::Result<()> {
        let directory = fs::symlink_metadata(&self.dir)?;
        if !directory.is_dir() || file_identity(&directory) != self.directory_identity {
            return Err(std::io::Error::other("capture directory identity changed"));
        }
        let mut failed = false;
        for (path, identity) in std::mem::take(&mut self.created) {
            let identity = match identity {
                CreatedIdentity::Known(identity) => identity,
                CreatedIdentity::Open(file) => {
                    // Initial fstat failed, but ownership was already registered. Recover the
                    // identity through that exact handle, then close it before unlinking.
                    let identity = file.metadata().map(|metadata| file_identity(&metadata));
                    drop(file);
                    match identity { Ok(identity) => identity, Err(_) => { failed = true; continue; } }
                }
            };
            match fs::symlink_metadata(&path) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Ok(metadata) if metadata.is_file() && file_identity(&metadata) == identity => {
                    if fs::remove_file(path).is_err() { failed = true; }
                }
                _ => failed = true, // a replacement/symlink is not this attempt's artifact
            }
        }
        if failed { Err(std::io::Error::other("capture rollback incomplete")) } else { Ok(()) }
    }
}

fn private_new_file(path: &Path) -> std::io::Result<fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    fs::OpenOptions::new().write(true).create_new(true).mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC).open(path)
}

/// An in-memory sink: every segment is a `Vec<u8>` the test reads back.
#[derive(Default)]
pub struct MemSink {
    pub manifest: String,
    pub segments: std::rc::Rc<std::cell::RefCell<Vec<Vec<u8>>>>,
}

struct MemSegment {
    store: std::rc::Rc<std::cell::RefCell<Vec<Vec<u8>>>>,
    index: usize,
}

impl Write for MemSegment {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.store.borrow_mut()[self.index].extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Sink for MemSink {
    fn segment(&mut self, index: u32) -> std::io::Result<Box<dyn Write>> {
        let mut s = self.segments.borrow_mut();
        while s.len() <= index as usize {
            s.push(Vec::new());
        }
        Ok(Box::new(MemSegment {
            store: self.segments.clone(),
            index: index as usize,
        }))
    }
    fn manifest(&mut self, text: &str) -> std::io::Result<()> {
        self.manifest = text.to_string();
        Ok(())
    }
    fn rollback(&mut self) -> std::io::Result<()> {
        self.manifest.clear();
        self.segments.borrow_mut().clear();
        Ok(())
    }
}

/// The writer: buffers one frame, writes once per frame, rotates and caps.
pub struct Writer {
    sink: Box<dyn Sink>,
    seg: Option<Box<dyn Write>>,
    seg_index: u32,
    seg_bytes: usize,
    total_bytes: usize,
    buf: Vec<u8>,
    stopped: bool,
    failure: Option<RecError>,
    frames: u64,
    /// Microseconds spent in the writer this second — the heartbeat's `rec=`.
    pub spent_us: u64,
}

impl Writer {
    /// Refuses to start anywhere but frame 0 (spec §5.3).
    pub fn open(mut sink: Box<dyn Sink>, header: &Header, at_frame: u64) -> Result<Self, RecError> {
        if at_frame != 0 {
            return Err(RecError::MidSession { at_frame });
        }
        let mut encoded = ManifestBuffer { bytes: Vec::with_capacity(4096), exceeded: false };
        if let Err(e) = serde_json::to_writer_pretty(&mut encoded, &header.wire()) {
            return Err(if encoded.exceeded { RecError::InitialTooLarge { limit: DATA_CAP_BYTES } }
                else { RecError::Io(e.to_string()) });
        }
        let text = String::from_utf8(encoded.bytes).map_err(|e| RecError::Io(e.to_string()))?;
        let opened = sink.manifest(&text).and_then(|()| sink.segment(0));
        let seg = match opened {
            Ok(seg) => seg,
            Err(error) => {
                let rollback = sink.rollback();
                return Err(RecError::Io(if rollback.is_err() { "recording open failed; rollback incomplete".into() }
                    else { error.to_string() }));
            }
        };
        Ok(Self {
            sink,
            seg: Some(seg),
            seg_index: 0,
            seg_bytes: 0,
            total_bytes: text.len(),
            buf: Vec::with_capacity(4096),
            stopped: false,
            failure: None,
            frames: 0,
            spent_us: 0,
        })
    }

    pub fn stopped(&self) -> bool {
        self.stopped
    }
    pub fn invalidate(&mut self, frame: u64) {
        self.line(json!({"f":frame,"t":"stopped","why":"unsupported"}));
        let _ = self.flush_frame();
        self.stopped = true;
    }

    fn line(&mut self, v: Value) {
        if self.stopped {
            return;
        }
        // serde_json's compact form: one line, no trailing space
        if let Ok(s) = serde_json::to_string(&v) {
            self.buf.extend_from_slice(s.as_bytes());
            self.buf.push(b'\n');
        }
    }

    pub fn tick(&mut self, f: u64, t: Tick) {
        self.line(json!({"f": f, "t": "tick", "ms": t.ms, "dt_us": t.dt_us}));
    }

    pub fn present(&mut self, f: u64, bit: bool, why: Option<&str>) {
        self.line(json!({"f": f, "t": "present", "bit": bit, "why": why}));
    }

    /// An input with its recorded resolution (`hit`, `resolved_focus` as the app encodes them).
    pub fn input(&mut self, f: u64, encoded: Value) {
        let mut v = json!({"f": f, "t": "in"});
        if let (Some(obj), Some(src)) = (v.as_object_mut(), encoded.as_object()) {
            for (k, x) in src {
                obj.insert(k.clone(), x.clone());
            }
        }
        self.line(v);
    }

    pub fn effect(&mut self, f: u64, from: &str, e: &str, addr: Option<(String, u32)>) {
        self.line(json!({"f": f, "t": "eff", "from": from, "e": e, "addr": addr.map(|(m, r)| json!({"to": m, "req": r}))}));
    }
    pub fn effect_payload(&mut self, f: u64, from: &str, e: &str, payload: Value) {
        self.line(json!({"f":f,"t":"eff","from":from,"e":e,"payload":payload}));
    }

    pub fn result(&mut self, f: u64, to: &str, req: u32, payload: Value) {
        self.line(json!({"f": f, "t": "async", "to": to, "req": req, "payload": payload}));
    }

    /// One STORE's landings on this frame (schema 2): `n` is HOW MANY of its sites consumed a
    /// mailbox, which is what `ui::landgate`'s cursor is stepped by, one unit per arrival. The
    /// count and not a boolean, because a store with several sites (`person` has one per fetch)
    /// can take two answers on one frame and one on the next, and collapsing that to "it landed"
    /// lets a replay consume two arrivals for one cursor step — after which every later landing
    /// of that store reads as late. `gen` is the store's notice generation after the frame:
    /// evidence for a reader, never something the gate keys on.
    pub fn land(&mut self, f: u64, ord: u32, gen: u32, n: u32) {
        self.line(json!({"f": f, "t": "land", "ord": ord, "gen": gen, "n": n}));
    }

    pub fn life(&mut self, f: u64, inst: u32, ev: &str) {
        self.line(json!({"f": f, "t": "life", "inst": inst, "ev": ev}));
    }

    pub fn timer(&mut self, f: u64, id: u32) {
        self.line(json!({"f": f, "t": "timer", "id": id}));
    }

    pub fn metrics(&mut self, f: u64, text: &str, sz: i32, bold: bool, w: f32, h: f32) {
        self.line(json!({"f": f, "t": "metrics", "s": text, "sz": sz, "b": bold, "w": w, "h": h}));
    }

    pub fn state(&mut self, f: u64, hash: u64) {
        self.line(json!({"f": f, "t": "st", "hash": hash}));
    }

    /// The engine's resolved focus after the frame's drains (`--resolve` replay grades it):
    /// `(entry, elem)` or none.
    pub fn focus(&mut self, f: u64, focus: Option<(u32, u32, Option<u32>)>) {
        self.line(json!({"f": f, "t": "fo", "entry": focus.map(|x| x.0), "elem": focus.map(|x| x.1), "group": focus.and_then(|x| x.2)}));
    }

    /// One write per frame. Rotates at `SEGMENT_BYTES`, stops at `CAP_BYTES` with a final note.
    pub fn flush_frame(&mut self) -> Result<(), RecError> {
        if let Some(error) = &self.failure { return Err(error.clone()); }
        let result = self.flush_frame_inner();
        if let Err(error) = &result {
            self.stopped = true;
            self.failure = Some(error.clone());
            self.buf.clear();
            self.seg = None;
        }
        result
    }

    fn flush_frame_inner(&mut self) -> Result<(), RecError> {
        self.frames += 1;
        if self.stopped || self.buf.is_empty() {
            return Ok(());
        }
        if self.buf.len() > DATA_CAP_BYTES.saturating_sub(self.total_bytes) {
            self.stopped = true;
            let note = format!(
                "{{\"f\":{},\"t\":\"stopped\",\"why\":\"cap\",\"bytes\":{}}}\n",
                self.frames - 1,
                self.total_bytes
            );
            debug_assert!(note.len() <= CAP_NOTE_RESERVE);
            self.buf = Vec::new();
            if let Some(seg) = self.seg.as_mut() {
                seg.write_all(note.as_bytes()).map_err(|e| RecError::Io(e.to_string()))?;
                self.seg_bytes += note.len();
                self.total_bytes += note.len();
                seg.flush().map_err(|e| RecError::Io(e.to_string()))?;
            }
            return Ok(());
        }
        if self.seg_bytes + self.buf.len() > SEGMENT_BYTES {
            if let Some(mut old) = self.seg.take() {
                old.flush().map_err(|e| RecError::Io(e.to_string()))?;
            }
            self.seg_index += 1;
            self.seg_bytes = 0;
            self.seg = Some(
                self.sink
                    .segment(self.seg_index)
                    .map_err(|e| RecError::Io(e.to_string()))?,
            );
        }
        let seg = self.seg.as_mut().ok_or_else(|| RecError::Io("segment unavailable".into()))?;
        seg.write_all(&self.buf)
            .map_err(|e| RecError::Io(e.to_string()))?;
        self.seg_bytes += self.buf.len();
        self.total_bytes += self.buf.len();
        self.buf.clear();
        Ok(())
    }

    pub(crate) fn abort(mut self) -> Result<(), RecError> {
        self.stopped = true;
        self.buf.clear();
        drop(self.seg.take()); // retire BufWriter/File before unlink; no later drop can flush
        self.sink.rollback().map_err(|_| RecError::Io("capture rollback incomplete".into()))
    }

    pub fn finish(mut self) -> Result<(), RecError> {
        self.flush_frame()?;
        if let Some(mut seg) = self.seg.take() {
            seg.flush().map_err(|e| RecError::Io(e.to_string()))?;
        }
        Ok(())
    }
}

/// One recorded frame, assembled from its lines.
#[derive(Clone, Debug, Default)]
pub struct Frame {
    pub f: u64,
    pub tick: Option<Tick>,
    pub present: Option<bool>,
    pub present_why: Option<String>,
    pub inputs: Vec<Value>,
    pub effects: Vec<Value>,
    pub results: Vec<Value>,
    /// The stores that consumed a landing on this frame, `(ordinal, generation, count)` (schema 2).
    pub lands: Vec<(u32, u32, u32)>,
    pub life: Vec<Value>,
    pub st: Option<u64>,
    /// The recorded focus after the drains: `Some(None)` is "recorded as none".
    pub focus: Option<Option<(u32, u32, Option<u32>)>>,
}

/// A loaded recording.
pub struct Recording {
    pub header: Header,
    pub frames: Vec<Frame>,
    /// `metrics{key: (w, h)}` — the side table replay's `TableMeasure` answers from.
    pub metrics: HashMap<(String, i32, bool), (f32, f32)>,
    pub stopped_at: Option<u64>,
}

impl Recording {
    /// Parse a manifest and its segments' bytes (in order). Refuses another schema or shape.
    pub fn parse(manifest: &str, segments: &[&[u8]], state_fp: u64) -> Result<Self, RecError> {
        Self::parse_budget(manifest, segments, state_fp, CAP_BYTES)
    }
    fn parse_budget(manifest: &str, segments: &[&[u8]], state_fp: u64, decoded_limit: usize) -> Result<Self, RecError> {
        input_budget(manifest.len(),segments.iter().map(|bytes| bytes.len()))?;
        let budget = strict_json::Budget::new(decoded_limit);
        for size in std::iter::once(manifest.len()).chain(segments.iter().map(|bytes| bytes.len())) {
            budget.charge(size.saturating_mul(3)).map_err(|what| malformed(0,what))?;
        }
        let mv = strict_json::decode_with(manifest.as_bytes(), &budget).map_err(|_| malformed(0,"manifest JSON/budget"))?;
        let header = Header::from_json(&mv)?;
        if header.schema != SCHEMA {
            return Err(RecError::Schema {
                theirs: header.schema,
                ours: SCHEMA,
            });
        }
        if header.state_fp != state_fp {
            return Err(RecError::StateShape {
                theirs: header.state_fp,
                ours: state_fp,
            });
        }
        let mut frames: Vec<Frame> = Vec::new();
        let mut metrics = HashMap::new();
        let mut stopped_at = None;
        let mut n = 0usize;
        for seg in segments {
            for line in seg.split(|&b| b == b'\n') {
                if line.is_empty() {
                    continue;
                }
                n += 1;
                let v = strict_json::decode_with(line, &budget).map_err(|_| malformed(n,"record JSON/budget"))?;
                let f = v["f"].as_u64().ok_or_else(|| malformed(n, "f"))?;
                let kind = v["t"].as_str().ok_or_else(|| malformed(n, "t"))?;
                if frames.last().map(|x| x.f) != Some(f) {
                    if frames.last().is_some_and(|frame| frame.f >= f) { return Err(malformed(n,"frame order")); }
                    if frames.last().is_some_and(|frame| frame.tick.is_none()) { return Err(malformed(n,"completed frame lacks tick")); }
                    budget.charge(4 * std::mem::size_of::<Frame>()).map_err(|what| malformed(n,what))?;
                    frames.push(Frame {
                        f,
                        ..Default::default()
                    });
                }
                let fr = frames.last_mut().expect("just pushed");
                match kind {
                    "tick" => {
                        if fr.tick.is_some() { return Err(malformed(n,"duplicate tick")); }
                        fr.tick = Some(Tick {
                            ms: checked_u32(&v,"ms",n)?,
                            dt_us: checked_u32(&v,"dt_us",n)?,
                        })
                    }
                    "present" => {
                        if fr.present.is_some() { return Err(malformed(n,"duplicate present")); }
                        fr.present = Some(v["bit"].as_bool().ok_or_else(|| malformed(n,"bit"))?);
                        fr.present_why = v["why"].as_str().map(String::from);
                    }
                    "in" => fr.inputs.push(v.clone()),
                    "eff" => fr.effects.push(v.clone()),
                    "async" => fr.results.push(v.clone()),
                    "land" => {
                        let ord = checked_u32(&v,"ord",n)?;
                        let generation = checked_u32(&v,"gen",n)?;
                        let count = checked_u32(&v,"n",n)?;
                        if count == 0 || fr.lands.iter().any(|(old,_,_)| *old == ord) {
                            return Err(malformed(n,"landing identity/count"));
                        }
                        fr.lands.push((ord,generation,count));
                    }
                    "life" => {
                        checked_u32(&v,"inst",n)?;
                        v["ev"].as_str().ok_or_else(|| malformed(n,"life event"))?;
                        fr.life.push(v.clone());
                    }
                    "timer" => { checked_u32(&v,"id",n)?; fr.life.push(v.clone()); }
                    "metrics" => {
                        let key = (v["s"].as_str().ok_or_else(|| malformed(n,"metric text"))?.to_owned(),
                            v["sz"].as_i64().and_then(|n| i32::try_from(n).ok()).ok_or_else(|| malformed(n,"metric size"))?,
                            v["b"].as_bool().ok_or_else(|| malformed(n,"metric bold"))?);
                        let metric = |field| -> Result<f32, RecError> {
                            let value = v[field].as_f64().ok_or_else(|| malformed(n,field))? as f32;
                            if !value.is_finite() { return Err(malformed(n,field)); }
                            Ok(value)
                        };
                        let value = (metric("w")?,metric("h")?);
                        if let Some((w,h)) = metrics.insert(key,value) {
                            if w.to_bits() != value.0.to_bits() || h.to_bits() != value.1.to_bits() {
                                return Err(malformed(n,"conflicting metrics"));
                            }
                        }
                    }
                    "st" => {
                        if fr.st.is_some() { return Err(malformed(n,"duplicate state")); }
                        fr.st = Some(v["hash"].as_u64().ok_or_else(|| malformed(n,"hash"))?);
                    }
                    "fo" => {
                        if fr.focus.is_some() { return Err(malformed(n,"duplicate focus")); }
                        fr.focus = Some(if v["entry"].is_null() && v["elem"].is_null() && v["group"].is_null() { None }
                            else { Some((checked_u32(&v,"entry",n)?,checked_u32(&v,"elem",n)?,
                                if v["group"].is_null() { None } else { Some(checked_u32(&v,"group",n)?) })) });
                    }
                    "stopped" => {
                        if stopped_at.replace(f).is_some() { return Err(malformed(n,"duplicate stopped")); }
                    }
                    _ => return Err(malformed(n, "record kind")),
                }
            }
        }
        if frames.last().is_some_and(|frame| frame.tick.is_none()) { return Err(malformed(n,"completed frame lacks tick")); }
        Ok(Self {
            header,
            frames,
            metrics,
            stopped_at,
        })
    }

    /// Load from a directory written by `DirSink`.
    pub fn load(dir: &Path, state_fp: u64) -> Result<Self, RecError> {
        let manifest = bounded_read(&dir.join("manifest.json"),CAP_BYTES)?;
        let manifest = String::from_utf8(manifest).map_err(|_| malformed(0,"manifest UTF-8"))?;
        let mut paths = Vec::new();
        let mut declared_size = manifest.len();
        for entry in fs::read_dir(dir).map_err(|e| RecError::Io(e.to_string()))? {
            let entry = entry.map_err(|e| RecError::Io(e.to_string()))?;
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("rec-") && name.ends_with(".jsonl") {
                if paths.len() >= CAP_BYTES / SEGMENT_BYTES + 1 { return Err(RecError::RecordingTooLarge {limit:CAP_BYTES}); }
                let metadata = fs::symlink_metadata(entry.path()).map_err(|_| RecError::Io("cannot inspect segment".into()))?;
                if !metadata.is_file() { return Err(RecError::Io("segment is not a regular file".into())); }
                let size = usize::try_from(metadata.len()).map_err(|_| RecError::RecordingTooLarge {limit:CAP_BYTES})?;
                input_budget(declared_size,[size])?;
                declared_size += size;
                paths.push((name,entry.path()));
            }
        }
        paths.sort_by(|a,b| a.0.cmp(&b.0));
        let mut remaining = CAP_BYTES - manifest.len();
        let mut segs = Vec::new();
        for (index,(name,path)) in paths.into_iter().enumerate() {
            if name != format!("rec-{index:04}.jsonl") { return Err(malformed(0,"segment sequence")); }
            let bytes = bounded_read(&path,remaining)?;
            remaining -= bytes.len();
            segs.push(bytes);
        }
        let refs: Vec<&[u8]> = segs.iter().map(Vec::as_slice).collect();
        Self::parse(&manifest, &refs, state_fp)
    }

    /// The LANDING SCHEDULE (§3.3 step 3): for each store ordinal, the `(frame, count)` pairs it
    /// was recorded consuming landings on, oldest first. This is what `ui::landgate::arm_replay`
    /// holds a replay's live landings to; an empty inner list means "this store never landed",
    /// which the gate reads as "deliver at once and grade it `extra`", never as "hold forever".
    pub fn land_schedule(&self) -> std::collections::BTreeMap<u32,Vec<(u64, u32)>> {
        let mut out = std::collections::BTreeMap::<u32,Vec<(u64,u32)>>::new();
        for frame in &self.frames {
            for (ord, _, n) in &frame.lands {
                out.entry(*ord).or_default().push((frame.f,*n));
            }
        }
        out
    }

    /// The `st` stream: `(frame, hash)` for every event frame.
    pub fn state_stream(&self) -> Vec<(u64, u64)> {
        self.frames
            .iter()
            .filter_map(|f| f.st.map(|h| (f.f, h)))
            .collect()
    }
}

/// The replay `Measure` (spec §4.3): answers from the recording's metrics table; a miss is a
/// HARD failure the replay driver reads after every frame.
pub struct TableMeasure {
    table: HashMap<(String, i32, bool), (f32, f32)>,
    miss: std::cell::Cell<Option<(String, i32, bool)>>,
}

impl TableMeasure {
    pub fn new(table: HashMap<(String, i32, bool), (f32, f32)>) -> Self {
        Self {
            table,
            miss: std::cell::Cell::new(None),
        }
    }

    /// The first key that was not in the table, if any (and clears it).
    pub fn take_miss(&self) -> Option<(String, i32, bool)> {
        self.miss.take()
    }
}

impl Measure for TableMeasure {
    fn width(&self, s: &CStr, sz: i32, bold: bool) -> f32 {
        let key = (s.to_string_lossy().to_string(), sz, bold);
        match self.table.get(&key) {
            Some((w, _)) => *w,
            None => {
                if self.miss.take().is_none() {
                    self.miss.set(Some(key));
                }
                0.0
            }
        }
    }
    fn cap_h(&self, sz: i32) -> f32 {
        sz as f32 * 0.7
    }
    fn line_h(&self, sz: i32) -> f32 {
        sz as f32 * 1.2
    }
}

/// The state SHAPE fingerprint (spec §5.4): a hash over the census of field names and types, per
/// store and per screen, so a new field re-fingerprints one machine. Each `LogicalState` type
/// declares its `SHAPE` as a string; the application folds them in a fixed order.
pub fn state_fp(shapes: &[&str]) -> u64 {
    let mut c = Canon::new();
    c.seq(shapes.len());
    for s in shapes {
        c.str(s);
    }
    c.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoded_budget_rejects_large_initial_collection() {
        let mut header = Header::new(17, &Init);
        header.init_data = json!({"collection": []});
        assert!(Recording::parse_budget(&header.to_json().to_string(), &[], 17, 64 * 1024).is_ok());
        header.init_data = json!({"collection": vec![0; 2048]});
        let manifest = serde_json::to_string(&header.to_json()).unwrap();
        assert!(Recording::parse_budget(&manifest, &[], 17, 64 * 1024).is_err());
    }
    #[test]
    fn decoded_budget_rejects_large_single_frame_collection() {
        let manifest = serde_json::to_string(&Header::new(17, &Init).to_json()).unwrap();
        let small = b"{\"f\":0,\"t\":\"tick\",\"ms\":0,\"dt_us\":0}\n{\"f\":0,\"t\":\"in\",\"payload\":[]}";
        assert!(Recording::parse_budget(&manifest, &[small], 17, 64 * 1024).is_ok());
        let rows = format!("{{\"f\":0,\"t\":\"tick\",\"ms\":0,\"dt_us\":0}}\n{}\n",
            json!({"f":0,"t":"in","payload":vec![0;2048]}));
        assert!(Recording::parse_budget(&manifest, &[rows.as_bytes()], 17, 64 * 1024).is_err());
    }
    #[test]
    fn decoded_budget_rejects_tiny_many_frame_rows() {
        let manifest = serde_json::to_string(&Header::new(17, &Init).to_json()).unwrap();
        assert!(Recording::parse_budget(&manifest, &[b"{\"f\":0,\"t\":\"tick\",\"ms\":0,\"dt_us\":0}"],17,128 * 1024).is_ok());
        let rows = (0..1024).map(|f| format!("{{\"f\":{f},\"t\":\"tick\",\"ms\":0,\"dt_us\":0}}\n")).collect::<String>();
        assert!(Recording::parse_budget(&manifest, &[rows.as_bytes()], 17, 128 * 1024).is_err());
    }
    #[test]
    fn completed_malformed_frame_is_rejected_before_next_rows() {
        let root = LoadRoot::new("missing-tick-early");
        root.rows(&[json!({"f":0,"t":"timer","id":1}),json!({"f":1,"t":"timer","id":1})]);
        assert!(Recording::load(&root.0, 17).is_err(), "completed frame without tick must be refused");
    }

    #[derive(Clone, Copy)]
    enum Fault { Write, Flush, Open }
    struct FaultSink(Fault);
    struct FaultSegment(Fault);
    impl Write for FaultSegment {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if matches!(self.0, Fault::Write) { return Err(std::io::Error::other("injected write failure")); }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            if matches!(self.0, Fault::Flush) { return Err(std::io::Error::other("injected flush failure")); }
            Ok(())
        }
    }
    impl Sink for FaultSink {
        fn manifest(&mut self, _: &str) -> std::io::Result<()> { Ok(()) }
        fn segment(&mut self, index: u32) -> std::io::Result<Box<dyn Write>> {
            if index > 0 && matches!(self.0, Fault::Open) { return Err(std::io::Error::other("injected open failure")); }
            Ok(Box::new(FaultSegment(self.0)))
        }
    }
    fn fault_writer(fault: Fault) -> Writer {
        Writer::open(Box::new(FaultSink(fault)), &Header::new(17, &Init), 0).unwrap()
    }
    fn assert_terminal(mut writer: Writer) {
        assert!(writer.flush_frame().is_err(), "storage failure must be surfaced");
        assert!(writer.stopped(), "storage failure must terminally latch");
        writer.tick(1, Tick { ms: 16, dt_us: 16000 });
        assert!(writer.flush_frame().is_err(), "later flush must retain failure without panic");
    }
    #[test]
    fn storage_midwrite_failure_is_terminal() {
        let mut writer = fault_writer(Fault::Write);
        writer.tick(0, Tick { ms: 0, dt_us: 0 });
        assert_terminal(writer);
    }
    #[test]
    fn storage_rotation_flush_failure_is_terminal() {
        let mut writer = fault_writer(Fault::Flush);
        writer.seg_bytes = SEGMENT_BYTES;
        writer.tick(0, Tick { ms: 0, dt_us: 0 });
        assert_terminal(writer);
    }
    #[test]
    fn storage_rotation_open_failure_is_terminal() {
        let mut writer = fault_writer(Fault::Open);
        writer.seg_bytes = SEGMENT_BYTES;
        writer.tick(0, Tick { ms: 0, dt_us: 0 });
        assert_terminal(writer);
    }

    struct LoadRoot(PathBuf);
    impl LoadRoot {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!("plxnative-record-load-{}-{tag}",std::process::id()));
            fs::create_dir(&path).expect("unique recording test directory");
            let root = Self(path);
            fs::write(root.0.join("manifest.json"),serde_json::to_vec(&Header::new(17,&Init).to_json()).unwrap()).unwrap();
            root
        }
        fn rows(&self, rows: &[Value]) {
            let text = rows.iter().map(|row| serde_json::to_string(row).unwrap()).collect::<Vec<_>>().join("\n");
            fs::write(self.0.join("rec-0000.jsonl"),text).unwrap();
        }
    }
    impl Drop for LoadRoot { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }

    #[test]
    fn production_load_rejects_truncated_tick_and_duplicate_tick() {
        let root = LoadRoot::new("checked-tick");
        let tick = json!({"f":0,"t":"tick","ms":37,"dt_us":16000});
        root.rows(&[tick.clone()]);
        assert_eq!(Recording::load(&root.0,17).unwrap().frames[0].tick.unwrap().ms,37);
        for field in ["ms","dt_us"] {
            let mut changed = tick.clone(); changed[field] = json!(tick[field].as_u64().unwrap() + (1u64<<32));
            root.rows(&[changed]);
            assert!(Recording::load(&root.0,17).is_err(),"overflow must not decode as the original tick");
            let mut changed = tick.clone(); changed.as_object_mut().unwrap().remove(field);
            root.rows(&[changed]);
            assert!(Recording::load(&root.0,17).is_err(),"missing time is not zero");
        }
        root.rows(&[tick.clone(),tick]);
        assert!(Recording::load(&root.0,17).is_err(),"duplicate tick cannot overwrite evidence");
    }

    #[test]
    fn production_load_rejects_truncated_landing_and_invalid_count() {
        let root = LoadRoot::new("checked-land");
        let tick = json!({"f":0,"t":"tick","ms":0,"dt_us":0});
        let landing = json!({"f":0,"t":"land","ord":1,"gen":9,"n":1});
        root.rows(&[tick.clone(),landing.clone()]);
        assert_eq!(Recording::load(&root.0,17).unwrap().frames[0].lands,vec![(1,9,1)]);
        for field in ["ord","gen","n"] {
            let mut changed = landing.clone(); changed[field] = json!(landing[field].as_u64().unwrap() + (1u64<<32));
            root.rows(&[tick.clone(),changed]);
            assert!(Recording::load(&root.0,17).is_err(),"overflow is not an equivalent landing");
        }
        let mut changed = landing; changed["n"] = json!(0);
        root.rows(&[tick,changed]);
        assert!(Recording::load(&root.0,17).is_err(),"zero is not an observed batch");
    }

    #[test]
    fn production_load_caps_manifest_and_aggregate_segments_before_json_parse() {
        let root = LoadRoot::new("aggregate-cap");
        let manifest = root.0.join("manifest.json");
        let saved = fs::read(&manifest).unwrap();
        fs::OpenOptions::new().write(true).open(&manifest).unwrap().set_len(CAP_BYTES as u64 + 1).unwrap();
        assert_eq!(Recording::load(&root.0,17).err(),Some(RecError::RecordingTooLarge {limit:CAP_BYTES}));
        fs::write(&manifest,saved).unwrap();
        for index in 0..2 {
            let file = fs::File::create(root.0.join(format!("rec-{index:04}.jsonl"))).unwrap();
            file.set_len((CAP_BYTES/2) as u64).unwrap(); // sparse; no 64 MiB fixture allocation
        }
        assert_eq!(Recording::load(&root.0,17).err(),Some(RecError::RecordingTooLarge {limit:CAP_BYTES}),
            "metadata rejects the aggregate before the sparse non-JSON bodies are read");
    }

    #[test]
    fn production_load_rejects_duplicate_json_keys_and_segment_symlinks() {
        let root = LoadRoot::new("duplicate-key");
        let segment = root.0.join("rec-0000.jsonl");
        fs::write(&segment,b"{\"f\":0,\"t\":\"tick\",\"ms\":4294967296,\"ms\":0,\"dt_us\":0}").unwrap();
        assert!(Recording::load(&root.0,17).is_err(),"duplicate key cannot erase overflowing evidence");
        fs::rename(&segment,root.0.join("retained.jsonl")).unwrap();
        fs::write(root.0.join("retained.jsonl"),b"{\"f\":0,\"t\":\"tick\",\"ms\":0,\"dt_us\":0}").unwrap();
        std::os::unix::fs::symlink(root.0.join("retained.jsonl"),&segment).unwrap();
        assert!(Recording::load(&root.0,17).is_err(),"a recording cannot redirect segment reads");
    }

    #[test]
    fn production_load_refuses_fifo_manifest_without_waiting_for_a_writer() {
        use std::os::unix::{ffi::OsStrExt,fs::OpenOptionsExt};
        let root = LoadRoot::new("fifo-manifest");
        let path = root.0.join("manifest.json");
        fs::rename(&path,root.0.join("retained-manifest.json")).unwrap();
        let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(),0o600) },0);
        let (tx,rx) = std::sync::mpsc::sync_channel(1);
        let directory = root.0.clone();
        let worker = std::thread::spawn(move || { tx.send(Recording::load(&directory,17).map(|_| ())).unwrap(); });
        let first = rx.recv_timeout(std::time::Duration::from_secs(2));
        let prompt = first.is_ok();
        let result = match first {
            Ok(result) => result,
            Err(_) => {
                // Regression cleanup: release a reader blocked in open, then join it before
                // failing. O_RDWR|O_NONBLOCK cannot itself wait for the missing peer.
                let _wake = fs::OpenOptions::new().read(true).write(true).custom_flags(libc::O_NONBLOCK).open(&path).unwrap();
                rx.recv_timeout(std::time::Duration::from_secs(2)).expect("FIFO reader released")
            }
        };
        worker.join().unwrap();
        assert!(prompt,"nonregular input must be refused without opening a blocking FIFO");
        assert!(matches!(result,Err(RecError::Io(_))));
    }

    #[test]
    fn full_width_store_ordinal_uses_sparse_schedule_storage() {
        let header = Header::new(17,&Init).to_json().to_string();
        let row = format!("{{\"f\":0,\"t\":\"tick\",\"ms\":0,\"dt_us\":0}}\n{{\"f\":0,\"t\":\"land\",\"ord\":{},\"gen\":{},\"n\":{}}}",u32::MAX,u32::MAX,u32::MAX);
        let record = Recording::parse(&header,&[row.as_bytes()],17).unwrap();
        let schedule = record.land_schedule();
        assert_eq!(schedule.len(),1);
        assert_eq!(schedule[&u32::MAX],vec![(0,u32::MAX)]);
        let _serial = crate::testlock::serial();
        let _gate = crate::ui::landgate::Armed;
        crate::ui::landgate::arm_sparse_replay(schedule);
        assert_eq!(crate::ui::landgate::unmatched_counts(),vec![(u32::MAX,0,u32::MAX)]);
    }

    #[test]
    fn disk_writer_is_private_from_creation_and_never_overwrites_a_capture() {
        use std::os::unix::fs::PermissionsExt;
        let root = LoadRoot::new("private-writer");
        let path = root.0.join("capture");
        let sink = DirSink::create(&path).unwrap();
        let writer = Writer::open(Box::new(sink),&Header::new(17,&Init),0).unwrap();
        writer.finish().unwrap();
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777,0o700);
        for name in ["manifest.json","rec-0000.jsonl"] {
            assert_eq!(fs::metadata(path.join(name)).unwrap().permissions().mode() & 0o777,0o600);
        }
        let before = fs::read(path.join("manifest.json")).unwrap();
        let sink = DirSink::create(&path).unwrap();
        assert!(Writer::open(Box::new(sink),&Header::new(19,&Init),0).is_err());
        assert_eq!(fs::read(path.join("manifest.json")).unwrap(),before);
    }

    #[test]
    fn failed_open_rolls_back_new_manifest_but_preserves_preexisting_segment() {
        let root = LoadRoot::new("open-rollback");
        let path = root.0.join("capture");
        fs::create_dir(&path).unwrap();
        fs::write(path.join("rec-0000.jsonl"),b"preexisting segment sentinel").unwrap();
        let sink = DirSink::create(&path).unwrap();
        assert!(Writer::open(Box::new(sink),&Header::new(17,&Init),0).is_err());
        assert!(!path.join("manifest.json").exists(), "partial open must not leave its new private header");
        assert_eq!(fs::read(path.join("rec-0000.jsonl")).unwrap(),b"preexisting segment sentinel");
    }

    #[test]
    fn initial_metadata_failure_keeps_an_owned_handle_for_open_rollback() {
        let root = LoadRoot::new("metadata-rollback");
        let path = root.0.join("capture");
        fs::create_dir(&path).unwrap();
        fs::write(path.join("preexisting-sentinel"),b"preserve existing").unwrap();
        let mut sink = DirSink::create(&path).unwrap();
        sink.fail_initial_metadata = true;
        assert!(Writer::open(Box::new(sink),&Header::new(17,&Init),0).is_err());
        assert!(!path.join("manifest.json").exists(), "the just-created file must not escape ownership on fstat failure");
        assert_eq!(fs::read(path.join("preexisting-sentinel")).unwrap(),b"preserve existing");
        let writer = Writer::open(Box::new(DirSink::create(&path).unwrap()),&Header::new(17,&Init),0)
            .expect("failed metadata lookup must not wedge immediate retry");
        writer.finish().unwrap();
    }

    #[test]
    fn rollback_refuses_replaced_artifacts_and_never_follows_symlink_targets() {
        let root = LoadRoot::new("rollback-identity");
        let path = root.0.join("capture");
        let target = root.0.join("external-sentinel");
        fs::write(&target,b"preserve external target").unwrap();
        let sink = DirSink::create(&path).unwrap();
        let mut writer = Writer::open(Box::new(sink),&Header::new(17,&Init),0).unwrap();
        writer.tick(0,Tick::default());
        fs::rename(path.join("manifest.json"),root.0.join("retained-own-header")).unwrap();
        std::os::unix::fs::symlink(&target,path.join("manifest.json")).unwrap();
        assert!(writer.abort().is_err(), "replacement is not an artifact this attempt owns");
        assert!(fs::symlink_metadata(path.join("manifest.json")).unwrap().file_type().is_symlink());
        assert_eq!(fs::read(target).unwrap(),b"preserve external target");
        assert!(!path.join("rec-0000.jsonl").exists(), "other still-owned artifacts are removed");
    }

    struct Init;
    impl LogicalState for Init {
        fn write(&self, w: &mut Canon) {
            w.u32(7);
        }
        fn probe(&self, out: &mut String) {
            out.push_str("seed=7");
        }
    }

    #[test]
    fn initial_manifest_counts_against_the_recording_cap() {
        let mut header = Header::new(1, &Init);
        header.init_data = json!({"state": "x".repeat(4096)});
        let bytes = serde_json::to_string_pretty(&header.to_json()).unwrap().len();
        let writer = Writer::open(Box::new(MemSink::default()), &header, 0).unwrap();
        assert_eq!(writer.total_bytes, bytes, "manifest bytes are part of the recording, not free storage");
    }

    #[test]
    fn oversized_initial_contents_are_refused_before_any_sink_write() {
        struct Untouched;
        impl Sink for Untouched {
            fn manifest(&mut self, _: &str) -> std::io::Result<()> { panic!("oversized manifest reached the sink") }
            fn segment(&mut self, _: u32) -> std::io::Result<Box<dyn Write>> { panic!("oversized recording opened a segment") }
        }
        let mut header = Header::new(1, &Init);
        for (text, count) in [("x", CAP_BYTES), ("\u{1}", CAP_BYTES / 5)] {
            header.init_data = Value::String(text.repeat(count));
            // The second source string is small enough, but JSON escaping exceeds the budget.
            assert_eq!(Writer::open(Box::new(Untouched), &header, 0).err(),
                Some(RecError::InitialTooLarge { limit: DATA_CAP_BYTES }));
        }
    }

    #[test]
    fn the_cap_note_fits_inside_the_same_budget_as_manifest_and_segments() {
        let sink = MemSink::default();
        let segments = sink.segments.clone();
        let mut writer = Writer::open(Box::new(sink), &Header::new(1, &Init), 0).unwrap();
        // Simulate already-written segments rather than allocating a 64 MiB fixture. Header
        // accounting is tested independently above; this tests the actual flush boundary.
        writer.total_bytes = DATA_CAP_BYTES - 3;
        let before = writer.total_bytes;
        writer.input(0, json!({"key": "ok"}));
        writer.flush_frame().unwrap();
        assert!(writer.stopped());
        let bytes = segments.borrow()[0].clone();
        let note: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(note["t"], "stopped");
        assert_eq!(note["bytes"], before);
        assert_eq!(writer.total_bytes, before + bytes.len());
        assert!(writer.total_bytes <= CAP_BYTES);
        writer.input(1, json!({"key": "ignored"}));
        writer.flush_frame().unwrap();
        assert_eq!(segments.borrow()[0], bytes, "no second marker or data after the cap");
    }

    #[test]
    fn a_recording_cannot_be_armed_mid_session() {
        let h = Header::new(1, &Init);
        let err = Writer::open(Box::new(MemSink::default()), &h, 12).err();
        assert_eq!(err, Some(RecError::MidSession { at_frame: 12 }));
        assert!(Writer::open(Box::new(MemSink::default()), &h, 0).is_ok());
    }

    #[test]
    fn a_recording_from_another_schema_is_refused() {
        let manifest = r#"{"schema": 99, "state_fp": 1}"#;
        let err = Recording::parse(manifest, &[], 1).err();
        assert_eq!(
            err,
            Some(RecError::Schema {
                theirs: 99,
                ours: SCHEMA
            })
        );
        let manifest = format!(r#"{{"schema": {SCHEMA}, "state_fp": 5}}"#);
        let err = Recording::parse(&manifest, &[], 1).err();
        assert_eq!(err, Some(RecError::StateShape { theirs: 5, ours: 1 }));
    }

    #[test]
    fn the_writer_round_trips_a_frame_and_rotates_segments() {
        let sink = MemSink::default();
        let segs = sink.segments.clone();
        let mut h = Header::new(3, &Init);
        h.init_data = json!({"fixture": [1, 2, 3]});
        let mut w = Writer::open(Box::new(sink), &h, 0).unwrap();
        w.tick(0, Tick { ms: 0, dt_us: 16 });
        w.present(0, true, Some("Input"));
        w.input(0, json!({"key": "ok", "hit": null}));
        w.state(0, 42);
        w.flush_frame().unwrap();
        // pad past one segment
        let big = "x".repeat(4000);
        for f in 1..600u64 {
            w.tick(f, Tick { ms: f as u32, dt_us: 16 });
            w.input(f, json!({"pad": big}));
            w.flush_frame().unwrap();
        }
        assert!(segs.borrow().len() >= 2, "rotated at SEGMENT_BYTES");
        let manifest = serde_json::to_string(&h.to_json()).unwrap();
        let s = segs.borrow();
        let refs: Vec<&[u8]> = s.iter().map(|v| v.as_slice()).collect();
        let r = Recording::parse(&manifest, &refs, 3).unwrap();
        assert_eq!(r.header.init_data, h.init_data);
        assert_eq!(r.frames[0].tick, Some(Tick { ms: 0, dt_us: 16 }));
        assert_eq!(r.frames[0].present, Some(true));
        assert_eq!(r.frames[0].present_why.as_deref(), Some("Input"));
        assert_eq!(r.frames[0].st, Some(42));
        assert_eq!(r.frames[0].inputs[0]["key"], "ok");
        assert_eq!(r.frames.len(), 600);
        assert_eq!(r.header.init_probe, "seed=7");
    }

    #[test]
    fn a_replay_measure_miss_fails_loudly() {
        let mut t = HashMap::new();
        t.insert(("Play".to_string(), 28, false), (61.0f32, 30.0f32));
        let m = TableMeasure::new(t);
        assert_eq!(m.width(c"Play", 28, false), 61.0);
        assert!(m.take_miss().is_none());
        let _ = m.width(c"Pause", 28, false);
        assert_eq!(m.take_miss(), Some(("Pause".to_string(), 28, false)));
    }

    #[test]
    fn the_state_shape_did_not_change_without_a_bump() {
        // The fixture bundle's shape (ui::fixture) is pinned in its own test; this pins the
        // census function so a reorder is a change.
        assert_ne!(state_fp(&["a:u32", "b:f32"]), state_fp(&["b:f32", "a:u32"]));
        assert_eq!(state_fp(&["a:u32"]), state_fp(&["a:u32"]));
    }
}
