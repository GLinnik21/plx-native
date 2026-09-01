//! Draw-phase instrumentation with three intentionally separate measurement modes — arm exactly
//! one per run.
//!
//! `/tmp/plxnative-profile` selects one phase for asynchronous
//! `GL_EXT_disjoint_timer_query` timing. It does not call `glFinish`; results are read on later
//! frames. `/tmp/plxnative-hwcnt` selects one phase for direct Mali Midgard vinstr attribution.
//! An empty trigger selects `frame.ui` in either mode. `/tmp/plxnative-cpuprof` is the third and
//! the only one that measures the CPU: the render thread's own inclusive wall time for EVERY
//! phase at once, no filter, no `glFinish`, logged as `PROFILE CPU` lines (a `~src` suffix marks
//! a phase met inside the blur source pass). It takes precedence in [`phase`] over the two GPU
//! modes, so arming it beside either silently pre-empts them.
//!
//! A selected HWCNT phase is delimited as:
//!
//! 1. `glFinish`, then a manual HWCNT dump that drains the reader's preceding interval;
//! 2. execute the phase;
//! 3. `glFinish`, then a second manual dump.
//!
//! r12p0 vinstr resets the hardware blocks on a dump and accumulates them per reader client. The
//! second buffer is therefore already the phase interval; it must **not** be subtracted from the
//! first as if both were absolute snapshots. Both raw buffers are retained in
//! `/tmp/plxnative-hwcnt.jsonl` so that assumption can be audited against the device data.
//!
//! The HWCNT run's serialized wall duration is calibration metadata only. It is not reported as
//! GPU time. GPU phase time comes from timer-query runs, while production p50/p95/worst-frame runs
//! are collected separately with both profilers off.

#[cfg(feature = "devtriggers")]
mod imp {
    use crate::hwcnt::{self, Sample, COUNTERS};
    use crate::log;
    use std::cell::RefCell;
    use std::fs::{File, OpenOptions};
    use std::io::{BufWriter, Write};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Instant;

    /// Every name `phase()` can be called with. Two of the wrappers compose their name from a loop
    /// index (`blur.reduce{1,2}`, `blur.tap{1,2}`), so this cannot be derived from the call sites
    /// and must be kept beside them. Its only job is to refuse a trigger that names nothing: an
    /// unknown filter arms the profiler, logs `on`, and then silently never matches — which reads
    /// exactly like "this phase costs nothing".
    pub(crate) const PHASES: [&str; 32] = [
        "profile.empty",
        "frame.ui",
        "main.ui",
        "blur.copy",
        // The direct path's own source pass — the one phase the capture path does not have.
        "blur.scene",
        "blur.reduce1",
        "blur.reduce2",
        "blur.tap1",
        "blur.tap2",
        "blur.up",
        "glass.composite",
        "glass.sharp",
        "glass.frost",
        "glass.foreground",
        "dt.about",
        "dt.backdrop",
        "dt.cast",
        "dt.ctitle",
        "dt.eps",
        "dt.hero",
        "dt.related",
        "dt.tabs",
        "hm.backdrop",
        "hm.chip",
        "hm.clear",
        "hm.grid",
        "hm.hero",
        "hm.layout",
        "hm.status",
        "hm.tabs",
        "menu.panel",
        "menu.scrim",
    ];

    /// Resolve a trigger's contents to a phase name, or explain why it is not one.
    fn select(filter: &str) -> Result<&'static str, String> {
        let wanted = match filter.trim() {
            "" => "frame.ui",
            name => name,
        };
        PHASES
            .iter()
            .find(|name| **name == wanted)
            .copied()
            .ok_or_else(|| {
                format!(
                    "unknown phase \"{wanted}\"; valid names: {}",
                    PHASES.join(" ")
                )
            })
    }

    const LOG_EVERY: u32 = 60;
    const NCOUNTERS: usize = COUNTERS.len();
    static ON: AtomicBool = AtomicBool::new(false);

    /// `/tmp/plxnative-cpuprof` — the THIRD mode, and the only one that measures the CPU.
    ///
    /// The timer-query and HWCNT modes price GPU work, and both are blind to the case the
    /// frame-drop detector reports as `draw=24ms swap=0.3ms`: a frame whose time is spent on the
    /// render thread ISSUING the page rather than on the GPU drawing it. This mode wraps every
    /// `phase` call in `Instant::now()` — nested phases are inclusive, and a phase met inside the
    /// blur SOURCE pass is accounted under its own `~src` key rather than folded into the visible
    /// pass — and logs one `PROFILE CPU phase=… n=… mean_ms=… max_ms=…` line per phase every
    /// [`LOG_EVERY`] drawn frames. No `glFinish`, no filter, every phase at once: two clock reads
    /// per phase is the whole perturbation, so it can run beside a scene's `fps=`.
    static CPU_ON: AtomicBool = AtomicBool::new(false);

    struct CpuAcc {
        name: &'static str,
        src: bool,
        samples: u32,
        total_ns: u128,
        max_ns: u128,
    }

    struct CpuState {
        frames: u32,
        acc: Vec<CpuAcc>,
    }

    thread_local! {
        static CPU: RefCell<CpuState> = const { RefCell::new(CpuState { frames: 0, acc: Vec::new() }) };
    }

    pub(crate) fn set_cpu_enabled() {
        CPU_ON.store(true, Ordering::Relaxed);
        log("PROFILE CPU on: every phase, inclusive wall time on the render thread, no glFinish");
    }

    #[inline]
    fn cpu_enabled() -> bool {
        CPU_ON.load(Ordering::Relaxed)
    }

    fn cpu_phase<R>(name: &'static str, draw: impl FnOnce() -> R) -> R {
        let src = crate::gfx::blur_source_pass();
        let t0 = Instant::now();
        let result = draw();
        let ns = t0.elapsed().as_nanos();
        CPU.with(|cpu| {
            let mut cpu = cpu.borrow_mut();
            match cpu.acc.iter_mut().find(|a| a.name == name && a.src == src) {
                Some(a) => {
                    a.samples += 1;
                    a.total_ns += ns;
                    a.max_ns = a.max_ns.max(ns);
                }
                None => cpu.acc.push(CpuAcc { name, src, samples: 1, total_ns: ns, max_ns: ns }),
            }
        });
        result
    }

    fn cpu_frame_end() {
        if !cpu_enabled() {
            return;
        }
        let messages = CPU.with(|cpu| {
            let mut cpu = cpu.borrow_mut();
            cpu.frames += 1;
            if cpu.frames < LOG_EVERY {
                return Vec::new();
            }
            let frames = cpu.frames;
            let mut out: Vec<String> = cpu
                .acc
                .iter()
                .map(|a| {
                    format!(
                        "PROFILE CPU phase={}{} n={} per_frame_ms={:.2} mean_ms={:.2} max_ms={:.2}",
                        a.name,
                        if a.src { "~src" } else { "" },
                        a.samples,
                        a.total_ns as f64 / frames as f64 / 1e6,
                        a.total_ns as f64 / a.samples.max(1) as f64 / 1e6,
                        a.max_ns as f64 / 1e6,
                    )
                })
                .collect();
            out.sort();
            cpu.acc.clear();
            cpu.frames = 0;
            out
        });
        for message in messages {
            log(&message);
        }
    }

    struct Acc {
        name: &'static str,
        wall_ns: u128,
        samples: u32,
        counters: [u128; NCOUNTERS],
        /// Min/max of `COUNTERS[0]` (GPU_ACTIVE) over the window, so the summary can say when its
        /// own mean is meaningless. See `gpu_timer::spread_note` for why this is not optional.
        active_min: u64,
        active_max: u64,
    }

    struct State {
        active: bool,
        in_phase: bool,
        filter: String,
        frames: u32,
        acc: Vec<Acc>,
        raw: Option<BufWriter<File>>,
        last_blur_config: Option<String>,
        logged_enables: bool,
    }

    impl State {
        const fn new() -> Self {
            Self {
                active: false,
                in_phase: false,
                filter: String::new(),
                frames: 0,
                acc: Vec::new(),
                raw: None,
                last_blur_config: None,
                logged_enables: false,
            }
        }

        fn selects(&self, name: &str) -> bool {
            self.active && self.filter == name
        }
    }

    thread_local! {
        static STATE: RefCell<State> = const { RefCell::new(State::new()) };
    }

    pub(crate) fn set_enabled(filter: &str) {
        let selected = match select(filter) {
            Ok(name) => name,
            Err(error) => {
                log(&format!("PROFILE GPU timer disabled: {error}"));
                return;
            }
        };
        if let Err(error) = crate::gpu_timer::init(selected) {
            log(&format!("PROFILE GPU timer disabled: {error}"));
        }
    }

    pub(crate) fn set_hwcnt_enabled(filter: &str) {
        ON.store(false, Ordering::Relaxed);
        // Validate BEFORE attaching: an unusable name must not leave a vinstr client holding the
        // counter hardware powered for a run that will never sample.
        let selected = match select(filter) {
            Ok(name) => name,
            Err(error) => {
                log(&format!("PROFILE disabled: {error}"));
                return;
            }
        };
        let info = match hwcnt::init() {
            Ok(info) => info,
            Err(e) => {
                log(&format!("PROFILE disabled: HWCNT init failed: {e}"));
                return;
            }
        };
        let path = crate::paths::in_runtime_dir("plxnative-hwcnt.jsonl");
        let file = match OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&path)
        {
            Ok(file) => file,
            Err(e) => {
                hwcnt::shutdown(); // the reader is already attached; do not leave it counting
                log(&format!("PROFILE disabled: {}: {e}", path.display()));
                return;
            }
        };

        let mut raw = BufWriter::new(file);
        let _ = writeln!(
            raw,
            "{{\"type\":\"info\",\"api\":{},\"hwver\":{},\"dump_size\":{},\"buffer_count\":{},\"map_size\":{},\"page_size\":{}}}",
            info.api, info.hwver, info.dump_size, info.buffer_count, info.map_size, info.page_size
        );
        STATE.with(|state| {
            let mut state = state.borrow_mut();
            state.active = true;
            state.in_phase = false;
            state.filter = selected.to_string();
            state.frames = 0;
            state.acc.clear();
            state.raw = Some(raw);
            state.last_blur_config = None;
            state.logged_enables = false;
        });
        ON.store(true, Ordering::Relaxed);
        log(&format!(
            "PROFILE HWCNT on: phase={selected} api={} hwver={} dump={} buffers={} map={} page={} raw={}",
            info.api,
            info.hwver,
            info.dump_size,
            info.buffer_count,
            info.map_size,
            info.page_size,
            path.display()
        ));
    }

    #[inline]
    fn enabled() -> bool {
        ON.load(Ordering::Relaxed)
    }

    struct PhaseGuard;

    impl Drop for PhaseGuard {
        fn drop(&mut self) {
            STATE.with(|state| state.borrow_mut().in_phase = false);
        }
    }

    fn enter(name: &'static str) -> Option<PhaseGuard> {
        if !enabled() || crate::gfx::blur_source_pass() {
            return None;
        }
        STATE.with(|state| {
            let mut state = state.borrow_mut();
            if !state.selects(name) || state.in_phase {
                return None;
            }
            state.in_phase = true;
            Some(PhaseGuard)
        })
    }

    fn disable_after_error(what: &str, error: String) {
        ON.store(false, Ordering::Relaxed);
        STATE.with(|state| state.borrow_mut().active = false);
        hwcnt::shutdown();
        log(&format!("PROFILE disabled: {what}: {error}"));
    }

    /// Time and count one selected draw phase; a passthrough when profiling or this phase is off.
    #[inline]
    fn hwcnt_phase<R>(name: &'static str, f: impl FnOnce() -> R) -> R {
        let Some(_guard) = enter(name) else {
            return f();
        };

        crate::gfx::gl_finish();
        let before = match hwcnt::sample() {
            Ok(sample) => sample,
            Err(e) => {
                disable_after_error("start sample", e);
                return f();
            }
        };
        note_block_enables(&before);
        let t0 = Instant::now();
        let result = f();
        crate::gfx::gl_finish();
        let wall_ns = t0.elapsed().as_nanos();
        let after = match hwcnt::sample() {
            Ok(sample) => sample,
            Err(e) => {
                disable_after_error("end sample", e);
                return result;
            }
        };

        record(name, wall_ns, &before, &after);
        result
    }

    /// Log each block's `PRFCNT_EN` mask once per run, from the first dump that arrives.
    ///
    /// A counter can read zero for two very different reasons — the work did not happen, or the
    /// hardware never enabled that counter group — and nothing else in the output distinguishes
    /// them. On this Mali-T820 the tiler block reports `0x1f`: only words 4..19 are live, so
    /// `TILER_ACTIVE` (word 22) is structurally zero and must not be read as "the tiler was idle".
    fn note_block_enables(sample: &Sample) {
        let message = STATE.with(|state| {
            let mut state = state.borrow_mut();
            if state.logged_enables {
                return None;
            }
            state.logged_enables = true;
            let masks = hwcnt::block_enables(sample);
            Some(format!(
                "PROFILE HWCNT prfcnt_en jm=0x{:x} tiler=0x{:x} l2=0x{:x} sc0=0x{:x} sc1=0x{:x} \
                 (each bit enables 4 counters; a word above the mask is DISABLED, not idle)",
                masks[0], masks[1], masks[2], masks[3], masks[4]
            ))
        });
        if let Some(message) = message {
            log(&message);
        }
    }

    fn write_words(raw: &mut BufWriter<File>, words: &[u32]) -> std::io::Result<()> {
        write!(raw, "[")?;
        for (i, word) in words.iter().enumerate() {
            if i != 0 {
                write!(raw, ",")?;
            }
            write!(raw, "{word}")?;
        }
        write!(raw, "]")
    }

    fn record(name: &'static str, wall_ns: u128, before: &Sample, interval: &Sample) {
        let decoded = hwcnt::decode(interval);
        STATE.with(|state| {
            let mut state = state.borrow_mut();
            match state.acc.iter_mut().find(|entry| entry.name == name) {
                Some(entry) => {
                    entry.wall_ns += wall_ns;
                    entry.samples += 1;
                    entry.active_min = entry.active_min.min(decoded[0]);
                    entry.active_max = entry.active_max.max(decoded[0]);
                    for (sum, value) in entry.counters.iter_mut().zip(decoded) {
                        *sum += value as u128;
                    }
                }
                None => state.acc.push(Acc {
                    name,
                    wall_ns,
                    samples: 1,
                    counters: decoded.map(u128::from),
                    active_min: decoded[0],
                    active_max: decoded[0],
                }),
            }

            if let Some(raw) = state.raw.as_mut() {
                let _ = write!(
                    raw,
                    "{{\"type\":\"phase\",\"name\":\"{name}\",\"serialized_wall_ns\":{wall_ns},\"start_gpu_ns\":{},\"end_gpu_ns\":{},\"start_event\":{},\"end_event\":{},\"load\":{},\"flush\":",
                    before.timestamp_ns, interval.timestamp_ns, before.event_id, interval.event_id,
                    // Which LOAD-DIAL step this sample belongs to, or -1. A cycled sweep is one
                    // JSONL holding several configurations; without this the distributions cannot
                    // be separated after the fact, and a mean over the whole file names a value
                    // that never occurred (the `SPREAD=` lesson, one level up).
                    crate::ui::glassload::step_index()
                );
                let _ = write_words(raw, &before.words);
                let _ = write!(raw, ",\"interval\":");
                let _ = write_words(raw, &interval.words);
                let _ = writeln!(raw, "}}");
                let _ = raw.flush();
            }
        });
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn note_blur_config(
        reg: [f32; 4],
        rx: i32,
        ry: i32,
        rw: i32,
        rh: i32,
        gw: i32,
        gh: i32,
        mw: i32,
        mh: i32,
        sw: i32,
        sh: i32,
    ) {
        // …or whenever the LOAD DIAL is armed. The region a leg actually blurred is the one thing
        // that cannot be recovered afterwards, and requiring a profiler to see it means it can only
        // be seen in runs whose frame rate the profiler has already changed.
        if !enabled() && !crate::gpu_timer::enabled() && !crate::ui::glassload::armed() {
            return;
        }
        let config = format!(
            "PROFILE blur_config authored={:.1},{:.1},{:.1},{:.1} aligned={rx},{ry},{rw},{rh} grab={gw}x{gh} mid={mw}x{mh} quarter={sw}x{sh}",
            reg[0], reg[1], reg[2], reg[3]
        );
        let changed = STATE.with(|state| {
            let mut state = state.borrow_mut();
            if state.last_blur_config.as_deref() == Some(&config) {
                return false;
            }
            state.last_blur_config = Some(config.clone());
            true
        });
        if changed {
            log(&config);
        }
    }

    /// Close one drawn-frame accounting interval and periodically emit decoded means.
    fn hwcnt_frame_end() {
        if !enabled() {
            return;
        }
        let messages = STATE.with(|state| {
            let mut state = state.borrow_mut();
            state.frames += 1;
            if state.frames < LOG_EVERY {
                return Vec::new();
            }

            let mut messages = Vec::with_capacity(state.acc.len());
            for entry in &state.acc {
                let count = entry.samples.max(1) as u128;
                let wall_ms = entry.wall_ns as f64 / count as f64 / 1_000_000.0;
                let ratio = entry.active_max as f64 / entry.active_min.max(1) as f64;
                let spread = if ratio >= 2.0 {
                    format!(" SPREAD={ratio:.1}x(GPU_ACTIVE {}..{}; the means below are NOT representative)",
                        entry.active_min, entry.active_max)
                } else {
                    String::new()
                };
                let mut message = format!(
                    "PROFILE HWCNT phase={} n={} serialized_wall_ms={wall_ms:.3}{spread}",
                    entry.name, entry.samples
                );
                for (spec, sum) in COUNTERS.iter().zip(entry.counters) {
                    message.push_str(&format!(" {}={}", spec.name, sum / count));
                }
                messages.push(message);
            }
            state.acc.clear();
            state.frames = 0;
            if let Some(raw) = state.raw.as_mut() {
                let _ = raw.flush();
            }
            messages
        });
        for message in messages {
            log(&message);
        }
    }

    #[inline]
    pub(crate) fn phase<R>(name: &'static str, draw: impl FnOnce() -> R) -> R {
        if cpu_enabled() {
            return cpu_phase(name, draw);
        }
        if crate::gpu_timer::enabled() {
            crate::gpu_timer::phase(name, draw)
        } else {
            hwcnt_phase(name, draw)
        }
    }

    pub(crate) fn frame_end() {
        crate::gpu_timer::frame_end();
        hwcnt_frame_end();
        cpu_frame_end();
    }
}

#[cfg(feature = "devtriggers")]
pub(crate) use imp::{frame_end, note_blur_config, phase, set_cpu_enabled, set_enabled, set_hwcnt_enabled};

#[cfg(not(feature = "devtriggers"))]
pub(crate) fn set_enabled(_filter: &str) {}

#[cfg(not(feature = "devtriggers"))]
pub(crate) fn set_hwcnt_enabled(_filter: &str) {}

#[cfg(not(feature = "devtriggers"))]
pub(crate) fn set_cpu_enabled() {}

#[cfg(not(feature = "devtriggers"))]
#[inline]
pub(crate) fn phase<R>(_name: &'static str, f: impl FnOnce() -> R) -> R {
    f()
}

#[cfg(not(feature = "devtriggers"))]
pub(crate) fn frame_end() {}

#[cfg(not(feature = "devtriggers"))]
#[allow(clippy::too_many_arguments)]
pub(crate) fn note_blur_config(
    _reg: [f32; 4],
    _rx: i32,
    _ry: i32,
    _rw: i32,
    _rh: i32,
    _gw: i32,
    _gh: i32,
    _mw: i32,
    _mh: i32,
    _sw: i32,
    _sh: i32,
) {
}
