//! Asynchronous GLES2 phase timing through `GL_EXT_disjoint_timer_query`.
//!
//! Query results are polled on later presented frames. The draw thread never waits for a result
//! and never calls `glFinish`. A target without the extension is unsupported rather than silently
//! falling back to CPU timing.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::ffi::{c_char, c_int, c_uint, c_void, CStr};
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::sync::atomic::{AtomicBool, Ordering};

const GL_EXTENSIONS: c_uint = 0x1F03;
const GL_TIME_ELAPSED_EXT: c_uint = 0x88BF;
const GL_QUERY_COUNTER_BITS_EXT: c_uint = 0x8864;
const GL_QUERY_RESULT_EXT: c_uint = 0x8866;
const GL_QUERY_RESULT_AVAILABLE_EXT: c_uint = 0x8867;
const GL_GPU_DISJOINT_EXT: c_uint = 0x8FBB;
const QUERY_COUNT: usize = 32;
const LOG_EVERY: u32 = 60;

type GenQueries = unsafe extern "C" fn(c_int, *mut c_uint);
type BeginQuery = unsafe extern "C" fn(c_uint, c_uint);
type EndQuery = unsafe extern "C" fn(c_uint);
type GetQueryIv = unsafe extern "C" fn(c_uint, c_uint, *mut c_int);
type GetQueryObjectUiv = unsafe extern "C" fn(c_uint, c_uint, *mut c_uint);
type GetQueryObjectUi64v = unsafe extern "C" fn(c_uint, c_uint, *mut u64);

#[derive(Clone, Copy)]
struct Api {
    begin: BeginQuery,
    end: EndQuery,
    get_query_iv: GetQueryIv,
    get_object_uiv: GetQueryObjectUiv,
    get_object_ui64v: GetQueryObjectUi64v,
}

struct Pending {
    id: c_uint,
    name: &'static str,
    issued_frame: u64,
    discard: bool,
}

struct Acc {
    name: &'static str,
    samples: u32,
    total_ns: u128,
    min_ns: u64,
    max_ns: u64,
}

struct State {
    api: Option<Api>,
    filter: String,
    in_phase: bool,
    frame: u64,
    free: Vec<c_uint>,
    pending: VecDeque<Pending>,
    raw: Option<BufWriter<File>>,
    acc: Vec<Acc>,
    samples_since_log: u32,
}

impl State {
    const fn new() -> Self {
        Self {
            api: None,
            filter: String::new(),
            in_phase: false,
            frame: 0,
            free: Vec::new(),
            pending: VecDeque::new(),
            raw: None,
            acc: Vec::new(),
            samples_since_log: 0,
        }
    }
}

thread_local! {
    static STATE: RefCell<State> = const { RefCell::new(State::new()) };
}

static ON: AtomicBool = AtomicBool::new(false);

extern "C" {
    fn SDL_GL_GetProcAddress(proc: *const c_char) -> *mut c_void;
    fn glGetString(name: c_uint) -> *const c_char;
    fn glGetIntegerv(name: c_uint, value: *mut c_int);
}

fn proc(name: &'static CStr) -> Result<*mut c_void, String> {
    let address = unsafe { SDL_GL_GetProcAddress(name.as_ptr()) };
    if address.is_null() {
        return Err(format!("{} is unavailable", name.to_string_lossy()));
    }
    Ok(address)
}

fn has_extension(name: &str) -> bool {
    let raw = unsafe { glGetString(GL_EXTENSIONS) };
    if raw.is_null() {
        return false;
    }
    let extensions = unsafe { CStr::from_ptr(raw) }.to_string_lossy();
    extensions.split_ascii_whitespace().any(|extension| extension == name)
}

pub(crate) fn init(filter: &str) -> Result<(), String> {
    ON.store(false, Ordering::Relaxed);
    if !has_extension("GL_EXT_disjoint_timer_query") {
        return Err("GL_EXT_disjoint_timer_query is not advertised".to_string());
    }

    let gen: GenQueries = unsafe { std::mem::transmute(proc(c"glGenQueriesEXT")?) };
    let begin: BeginQuery = unsafe { std::mem::transmute(proc(c"glBeginQueryEXT")?) };
    let end: EndQuery = unsafe { std::mem::transmute(proc(c"glEndQueryEXT")?) };
    let get_query_iv: GetQueryIv = unsafe { std::mem::transmute(proc(c"glGetQueryivEXT")?) };
    let get_object_uiv: GetQueryObjectUiv =
        unsafe { std::mem::transmute(proc(c"glGetQueryObjectuivEXT")?) };
    let get_object_ui64v: GetQueryObjectUi64v =
        unsafe { std::mem::transmute(proc(c"glGetQueryObjectui64vEXT")?) };
    let api = Api { begin, end, get_query_iv, get_object_uiv, get_object_ui64v };

    let mut bits = 0;
    unsafe { (api.get_query_iv)(GL_TIME_ELAPSED_EXT, GL_QUERY_COUNTER_BITS_EXT, &mut bits) };
    if bits < 30 {
        return Err(format!("TIME_ELAPSED has only {bits} counter bits"));
    }

    let mut ids = [0; QUERY_COUNT];
    unsafe { gen(QUERY_COUNT as c_int, ids.as_mut_ptr()) };
    if ids.iter().any(|id| *id == 0) {
        return Err("glGenQueriesEXT returned query 0".to_string());
    }

    // Reading the flag establishes the start of the first validity interval.
    let mut ignored = 0;
    unsafe { glGetIntegerv(GL_GPU_DISJOINT_EXT, &mut ignored) };

    let path = crate::paths::in_runtime_dir("plxnative-gputime.jsonl");
    let file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&path)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    let mut raw = BufWriter::new(file);
    writeln!(
        raw,
        "{{\"type\":\"info\",\"api\":\"GL_EXT_disjoint_timer_query\",\"counter_bits\":{bits},\"query_count\":{QUERY_COUNT}}}"
    )
    .map_err(|error| format!("{}: {error}", path.display()))?;
    raw.flush().map_err(|error| format!("{}: {error}", path.display()))?;

    let selected = match filter.trim() {
        "" => "frame.ui",
        name => name,
    };
    STATE.with(|slot| {
        let mut state = slot.borrow_mut();
        state.api = Some(api);
        state.filter = selected.to_string();
        state.in_phase = false;
        state.frame = 0;
        state.free = ids.into_iter().rev().collect();
        state.pending.clear();
        state.raw = Some(raw);
        state.acc.clear();
        state.samples_since_log = 0;
    });
    ON.store(true, Ordering::Relaxed);
    crate::log(&format!(
        "PROFILE GPU timer on: phase={selected} bits={bits} queries={QUERY_COUNT} raw={}",
        path.display()
    ));
    Ok(())
}

#[inline]
pub(crate) fn enabled() -> bool {
    ON.load(Ordering::Relaxed)
}

fn begin(name: &'static str) -> Option<c_uint> {
    // The page draws a second time as the direct path's blur source. Its own phases would then
    // record twice per frame under one name — a quarter-resolution sample averaged into the
    // full-resolution mean the experiment is priced by. `blur.scene` itself opens BEFORE the flag
    // is armed, so the source pass is still measurable as a whole.
    if !enabled() || crate::gfx::blur_source_pass() {
        return None;
    }
    STATE.with(|slot| {
        let mut state = slot.borrow_mut();
        if state.filter != name || state.in_phase {
            return None;
        }
        // Resolve the API first: popping an id and then bailing would retire that query object
        // for the rest of the run, and the free list is the only pool there is.
        let api = state.api?;
        let id = state.free.pop()?;
        unsafe { (api.begin)(GL_TIME_ELAPSED_EXT, id) };
        state.in_phase = true;
        Some(id)
    })
}

fn end(id: c_uint, name: &'static str, discard: bool) {
    STATE.with(|slot| {
        let mut state = slot.borrow_mut();
        let Some(api) = state.api else { return };
        unsafe { (api.end)(GL_TIME_ELAPSED_EXT) };
        state.in_phase = false;
        let issued_frame = state.frame;
        state.pending.push_back(Pending { id, name, issued_frame, discard });
    });
}

struct QueryGuard {
    id: c_uint,
    name: &'static str,
    armed: bool,
}

impl Drop for QueryGuard {
    fn drop(&mut self) {
        if self.armed {
            end(self.id, self.name, true);
        }
    }
}

#[inline]
pub(crate) fn phase<R>(name: &'static str, draw: impl FnOnce() -> R) -> R {
    let Some(id) = begin(name) else { return draw() };
    let mut guard = QueryGuard { id, name, armed: true };
    let result = draw();
    // Submit before closing the interval. Midgard defers a render target's fragment work until
    // that target's pass is flushed, so an interval closed with the pass still open contains no
    // submitted job: measured on the television, `glass.composite` and `glass.frost` — both
    // full-resolution quads into framebuffer 0 — reported 0.001 ms, while the frame containing
    // them reported 21.7. `glFlush`, never `glFinish`: this path must not stall the CPU, and it
    // runs only for the ONE selected phase, so the production frame is untouched.
    crate::gfx::gl_flush();
    end(id, name, false);
    guard.armed = false;
    result
}

/// Warn, in the log line itself, when a window's samples do not cluster.
///
/// A mean is only a summary of a distribution that HAS a middle. This phase's samples can be
/// bimodal — `blur.scene` on the direct backdrop path lands at either ~0.2 ms or ~2.0 ms with
/// nothing between — and a mean then names a value that never occurred. Two opposite and equally
/// wrong conclusions were drawn from single log lines of that shape before this existed, so the
/// spread is printed beside the mean rather than left for whoever reads the JSONL later.
fn spread_note(min_ns: u64, max_ns: u64) -> String {
    let ratio = max_ns as f64 / min_ns.max(1) as f64;
    if ratio >= 2.0 {
        format!(" SPREAD={ratio:.1}x (mean is not representative; read the jsonl)")
    } else {
        String::new()
    }
}

fn record(state: &mut State, pending: Pending, gpu_ns: u64) {
    if let Some(raw) = state.raw.as_mut() {
        let _ = writeln!(
            raw,
            "{{\"type\":\"timer\",\"name\":\"{}\",\"gpu_ns\":{gpu_ns},\"issued_frame\":{},\"collected_frame\":{}}}",
            pending.name, pending.issued_frame, state.frame
        );
        let _ = raw.flush();
    }
    match state.acc.iter_mut().find(|entry| entry.name == pending.name) {
        Some(entry) => {
            entry.samples += 1;
            entry.total_ns += gpu_ns as u128;
            entry.min_ns = entry.min_ns.min(gpu_ns);
            entry.max_ns = entry.max_ns.max(gpu_ns);
        }
        None => state.acc.push(Acc {
            name: pending.name,
            samples: 1,
            total_ns: gpu_ns as u128,
            min_ns: gpu_ns,
            max_ns: gpu_ns,
        }),
    }
    state.samples_since_log += 1;
}

pub(crate) fn frame_end() {
    if !enabled() {
        return;
    }
    let mut disjoint = 0;
    unsafe { glGetIntegerv(GL_GPU_DISJOINT_EXT, &mut disjoint) };
    let (messages, disjoint_message) = STATE.with(|slot| {
        let mut state = slot.borrow_mut();
        if disjoint != 0 {
            for pending in &mut state.pending {
                pending.discard = true;
            }
            let frame = state.frame;
            if let Some(raw) = state.raw.as_mut() {
                let _ = writeln!(raw, "{{\"type\":\"disjoint\",\"frame\":{frame}}}");
                let _ = raw.flush();
            }
        }

        loop {
            let Some(front) = state.pending.front() else { break };
            let Some(api) = state.api else { break };
            let mut available = 0;
            unsafe { (api.get_object_uiv)(front.id, GL_QUERY_RESULT_AVAILABLE_EXT, &mut available) };
            if available == 0 {
                break;
            }
            let pending = state.pending.pop_front().expect("front just existed");
            let mut gpu_ns = 0u64;
            unsafe { (api.get_object_ui64v)(pending.id, GL_QUERY_RESULT_EXT, &mut gpu_ns) };
            state.free.push(pending.id);
            if !pending.discard {
                record(&mut state, pending, gpu_ns);
            }
        }

        let mut messages = Vec::new();
        if state.samples_since_log >= LOG_EVERY {
            for entry in &state.acc {
                let mean_ms = entry.total_ns as f64 / entry.samples as f64 / 1_000_000.0;
                messages.push(format!(
                    "PROFILE timer phase={} n={} gpu_ms={mean_ms:.3} min_ms={:.3} max_ms={:.3}{}",
                    entry.name,
                    entry.samples,
                    entry.min_ns as f64 / 1_000_000.0,
                    entry.max_ns as f64 / 1_000_000.0,
                    spread_note(entry.min_ns, entry.max_ns)
                ));
            }
            state.acc.clear();
            state.samples_since_log = 0;
        }
        // Advance only now, so `issued_frame` and `collected_frame` are stamped on one base: a
        // query issued and collected in the same call reads as a zero-frame latency, not one.
        state.frame += 1;
        (messages, disjoint != 0)
    });
    if disjoint_message {
        crate::log("PROFILE timer: GPU disjoint; pending timer results discarded");
    }
    for message in messages {
        crate::log(&message);
    }
}
