//! The recorder and the recording (restructure spec §5.3–§5.5): what a controlled boot writes,
//! how it is bounded, and how it is read back for replay.
//!
//! **Format.** A directory: `manifest.json` (the header) and `rec-NNNN.jsonl` segments, one JSON
//! object per line, `{"f":<frame>,"t":"<kind>",…}`. Kinds per frame: `tick` (EVERY frame),
//! `present` (the bit and WHY), `in` (an input, with its recorded resolution), `eff` (an effect,
//! `from`/`e`/`addr`), `async` (an adapter result's address and payload or blob hash), `life`,
//! `timer`, `st` (the logical-state hash, on every EVENT frame). The header carries `schema`,
//! `state_fp`, the build, the features, the armed triggers, `init` (the application's initial
//! conditions, `H::Init`) and the clock origin.
//!
//! **Bounds.** Buffered — one write per frame at most; segment rotation at 2 MiB; a HARD byte cap
//! (64 MB: the runtime root is a RAM-backed tmpfs on the set) after which the recording STOPS and
//! says so; directory 0700, files 0600. No `scrub_local` on this path: the directory is private
//! (`.gitignore`, `outbound-guard.py`'s `PRIVATE_DIRS`).
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
use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::machine::{Canon, LogicalState, Measure, Tick};

/// The record format's version. A recording from another schema is REFUSED, both printed.
pub const SCHEMA: u32 = 1;
/// Segment rotation.
pub const SEGMENT_BYTES: usize = 2 * 1024 * 1024;
/// The hard cap on one recording (spec §5.3, settled on the tmpfs measurement).
pub const CAP_BYTES: usize = 64 * 1024 * 1024;
// Enough for the final cap record with full-width frame/byte counters. The manifest and normal
// segments share the remaining budget; stopping must not itself exceed the hard cap.
const CAP_NOTE_RESERVE: usize = 128;
const DATA_CAP_BYTES: usize = CAP_BYTES - CAP_NOTE_RESERVE;

/// What a recording is refused for.
#[derive(Debug, PartialEq, Eq)]
pub enum RecError {
    /// `Writer::open` was asked to start anywhere but frame 0.
    MidSession { at_frame: u64 },
    InitialTooLarge { limit: usize },
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

    fn to_json(&self) -> Value {
        serde_json::to_value(self.wire()).expect("header wire contains only JSON values")
    }

    fn from_json(v: &Value) -> Result<Self, RecError> {
        let get_u64 = |k: &str| v.get(k).and_then(Value::as_u64);
        Ok(Self {
            schema: get_u64("schema").ok_or_else(|| malformed(0, "schema"))? as u32,
            state_fp: get_u64("state_fp").ok_or_else(|| malformed(0, "state_fp"))?,
            build: v["build"].as_str().unwrap_or("").to_string(),
            features: strings(&v["features"]),
            triggers: strings(&v["triggers"]),
            init_probe: v["init"]["probe"].as_str().unwrap_or("").to_string(),
            init_hash: v["init"]["hash"].as_u64().unwrap_or(0),
            init_data: v["init"].get("data").cloned().unwrap_or(Value::Null),
            clock_start_ms: v["clock"]["start"].as_u64().unwrap_or(0) as u32,
            blobs: v["blobs"].as_bool().unwrap_or(false),
        })
    }
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default()
}

fn malformed(line: usize, what: &str) -> RecError {
    RecError::Malformed {
        line,
        what: what.to_string(),
    }
}

/// Where the bytes go: a directory of segments (the product), or memory (tests).
pub trait Sink {
    fn segment(&mut self, index: u32) -> std::io::Result<Box<dyn Write>>;
    fn manifest(&mut self, text: &str) -> std::io::Result<()>;
}

/// The product sink: `dir/manifest.json`, `dir/rec-NNNN.jsonl`, private modes.
pub struct DirSink {
    dir: PathBuf,
}

impl DirSink {
    pub fn create(dir: &Path) -> std::io::Result<Self> {
        fs::create_dir_all(dir)?;
        private_mode(dir, 0o700)?;
        Ok(Self {
            dir: dir.to_path_buf(),
        })
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
        let f = fs::File::create(&p)?;
        private_mode(&p, 0o600)?;
        Ok(Box::new(std::io::BufWriter::new(f)))
    }

    fn manifest(&mut self, text: &str) -> std::io::Result<()> {
        let p = self.dir.join("manifest.json");
        fs::write(&p, text)?;
        private_mode(&p, 0o600)
    }
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
        sink.manifest(&text).map_err(|e| RecError::Io(e.to_string()))?;
        let seg = sink.segment(0).map_err(|e| RecError::Io(e.to_string()))?;
        Ok(Self {
            sink,
            seg: Some(seg),
            seg_index: 0,
            seg_bytes: 0,
            total_bytes: text.len(),
            buf: Vec::with_capacity(4096),
            stopped: false,
            frames: 0,
            spent_us: 0,
        })
    }

    pub fn stopped(&self) -> bool {
        self.stopped
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

    pub fn result(&mut self, f: u64, to: &str, req: u32, payload: Value) {
        self.line(json!({"f": f, "t": "async", "to": to, "req": req, "payload": payload}));
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
                let _ = old.flush();
            }
            self.seg_index += 1;
            self.seg_bytes = 0;
            self.seg = Some(
                self.sink
                    .segment(self.seg_index)
                    .map_err(|e| RecError::Io(e.to_string()))?,
            );
        }
        let seg = self.seg.as_mut().expect("a segment is open");
        seg.write_all(&self.buf)
            .map_err(|e| RecError::Io(e.to_string()))?;
        self.seg_bytes += self.buf.len();
        self.total_bytes += self.buf.len();
        self.buf.clear();
        Ok(())
    }

    pub fn finish(mut self) {
        let _ = self.flush_frame();
        if let Some(mut seg) = self.seg.take() {
            let _ = seg.flush();
        }
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
        let mv: Value = serde_json::from_str(manifest).map_err(|e| RecError::Io(e.to_string()))?;
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
                let v: Value = serde_json::from_slice(line).map_err(|e| RecError::Malformed {
                    line: n,
                    what: e.to_string(),
                })?;
                let f = v["f"].as_u64().ok_or_else(|| malformed(n, "f"))?;
                let kind = v["t"].as_str().ok_or_else(|| malformed(n, "t"))?;
                if frames.last().map(|x| x.f) != Some(f) {
                    frames.push(Frame {
                        f,
                        ..Default::default()
                    });
                }
                let fr = frames.last_mut().expect("just pushed");
                match kind {
                    "tick" => {
                        fr.tick = Some(Tick {
                            ms: v["ms"].as_u64().unwrap_or(0) as u32,
                            dt_us: v["dt_us"].as_u64().unwrap_or(0) as u32,
                        })
                    }
                    "present" => {
                        fr.present = v["bit"].as_bool();
                        fr.present_why = v["why"].as_str().map(String::from);
                    }
                    "in" => fr.inputs.push(v.clone()),
                    "eff" => fr.effects.push(v.clone()),
                    "async" => fr.results.push(v.clone()),
                    "life" | "timer" => fr.life.push(v.clone()),
                    "metrics" => {
                        metrics.insert(
                            (
                                v["s"].as_str().unwrap_or("").to_string(),
                                v["sz"].as_i64().unwrap_or(0) as i32,
                                v["b"].as_bool().unwrap_or(false),
                            ),
                            (
                                v["w"].as_f64().unwrap_or(0.0) as f32,
                                v["h"].as_f64().unwrap_or(0.0) as f32,
                            ),
                        );
                    }
                    "st" => fr.st = v["hash"].as_u64(),
                    "fo" => {
                        fr.focus = Some(match (v["entry"].as_u64(), v["elem"].as_u64()) {
                            (Some(e), Some(k)) => Some((e as u32, k as u32, v["group"].as_u64().map(|g| g as u32))),
                            _ => None,
                        })
                    }
                    "stopped" => stopped_at = Some(f),
                    other => return Err(malformed(n, other)),
                }
            }
        }
        Ok(Self {
            header,
            frames,
            metrics,
            stopped_at,
        })
    }

    /// Load from a directory written by `DirSink`.
    pub fn load(dir: &Path, state_fp: u64) -> Result<Self, RecError> {
        let manifest = fs::read_to_string(dir.join("manifest.json")).map_err(|e| RecError::Io(e.to_string()))?;
        let mut segs: Vec<(String, Vec<u8>)> = Vec::new();
        for entry in fs::read_dir(dir).map_err(|e| RecError::Io(e.to_string()))? {
            let entry = entry.map_err(|e| RecError::Io(e.to_string()))?;
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("rec-") && name.ends_with(".jsonl") {
                segs.push((name, fs::read(entry.path()).map_err(|e| RecError::Io(e.to_string()))?));
            }
        }
        segs.sort_by(|a, b| a.0.cmp(&b.0));
        let refs: Vec<&[u8]> = segs.iter().map(|(_, b)| b.as_slice()).collect();
        Self::parse(&manifest, &refs, state_fp)
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
