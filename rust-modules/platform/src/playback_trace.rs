//! Opt-in, process-local firmware playback tracing. No worker or developer trigger is needed.
//! GStreamer public logging ABI: NDK gst/gstinfo.h; symbols checked in DAS firmware 3–6/9–11.
//! No native struct layout is read. Missing APIs refuse recording, never playback.
use std::ffi::c_void;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// A diagnostic resource bound, not a limit on library or other viewer content.
pub const TRACE_BYTE_LIMIT: usize = 8 * 1024 * 1024;
pub const TRACE_SECONDS: u64 = 60;
const LINE_BYTE_LIMIT: usize = 4096;
const SPEC: &std::ffi::CStr = c"dvbin:6,dvsplitter:6,dvsplitter_algo:6,dvmdparse:6,dualsequencer:6,lxvideodec:6,lxvideosink:6";

type LogFn = unsafe extern "C" fn(
    *mut c_void,
    i32,
    *const libc::c_char,
    *const libc::c_char,
    i32,
    *mut c_void,
    *mut c_void,
    *mut c_void,
);
plx_base::dynlib! {
    gst: ["libgstreamer-1.0.so.0"] {
        fn gst_debug_add_log_function(f: Option<LogFn>, data: *mut c_void, notify: Option<unsafe extern "C" fn(*mut c_void)>);
        fn gst_debug_remove_log_function(f: Option<LogFn>) -> u32;
        fn gst_debug_set_active(active: i32);
        fn gst_debug_set_threshold_from_string(spec: *const libc::c_char, reset: i32);
        fn gst_debug_message_get(message: *mut c_void) -> *const libc::c_char;
        fn gst_debug_category_get_name(category: *mut c_void) -> *const libc::c_char;
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Status {
    #[default]
    Off,
    Recording,
    Saved,
    Exported,
    Saving,
    Unavailable,
    Failed,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    Start,
    Stop,
    Status,
    Export,
    Delete,
}

/// The exact size/time guard used by the live writer and tests. Time is an input here.
struct Capture<W> {
    writer: W,
    bytes: usize,
    limit: usize,
    deadline: Duration,
    active: bool,
}
impl<W: Write> Capture<W> {
    fn new(writer: W, limit: usize, deadline: Duration) -> Self {
        Self {
            writer,
            bytes: 0,
            limit,
            deadline,
            active: true,
        }
    }
    fn push(&mut self, elapsed: Duration, line: &[u8]) -> std::io::Result<()> {
        if elapsed >= self.deadline {
            self.active = false;
        }
        if !self.active {
            return Ok(());
        }
        let count = line.len().min(self.limit.saturating_sub(self.bytes));
        if let Err(error) = self.writer.write_all(&line[..count]) {
            self.active = false;
            return Err(error);
        }
        self.bytes += count;
        if self.bytes == self.limit {
            self.active = false;
        }
        Ok(())
    }
}
#[derive(Default)]
struct State {
    status: Status,
    capture: Option<(Instant, Capture<File>)>,
    registered: bool,
    generation: u64,
}
impl State {
    fn invalidate(&mut self) {
        self.generation = self.generation.wrapping_add(1);
    }
}
static STATE: Mutex<State> = Mutex::new(State {
    status: Status::Off,
    capture: None,
    registered: false,
    generation: 0,
});
static EXPORT_IO: Mutex<()> = Mutex::new(());
static API: OnceLock<bool> = OnceLock::new();
static ACTIVE: AtomicBool = AtomicBool::new(false);

fn path() -> std::path::PathBuf {
    plx_base::paths::in_runtime_dir(plx_base::paths::runtime_file::GST)
}
fn available() -> bool {
    *API.get_or_init(|| gst::load(None).ok())
}

/// Run only in the application's effect runner. Every command returns an addressed UI answer.
pub fn execute(command: Command) -> Status {
    tick();
    if command == Command::Status {
        return STATE.lock().unwrap_or_else(|e| e.into_inner()).status;
    }
    if command == Command::Export {
        return save_export(prepare_export());
    }
    let _io = EXPORT_IO.lock().unwrap_or_else(|e| e.into_inner());
    if command == Command::Stop || command == Command::Delete {
        stop();
        let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
        if command == Command::Delete {
            let removed = [path(), export_path()]
                .iter()
                .map(|path| match std::fs::remove_file(path) {
                    Ok(()) => true,
                    Err(error) => error.kind() == std::io::ErrorKind::NotFound,
                })
                .fold(true, |ok, next| ok & next);
            state.status = if removed { Status::Off } else { Status::Failed };
        }
        return state.status;
    }
    if !available() {
        return Status::Unavailable;
    }
    // Stop the previous capture before touching its file or installing logging callbacks.
    stop();
    let file = plx_base::eventlog::open_log_append(&path()).and_then(|file| {
        file.set_len(0)?;
        Ok(file)
    });
    let Ok(file) = file else {
        return Status::Failed;
    };
    let mut capture = Capture::new(file, TRACE_BYTE_LIMIT, Duration::from_secs(TRACE_SECONDS));
    if capture
        .push(
            Duration::ZERO,
            b"PlxNative playback trace: limit=8MiB duration=60s; local diagnostic recording\n",
        )
        .is_err()
    {
        return Status::Failed;
    }
    let register = {
        let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
        state.invalidate();
        state.capture = Some((Instant::now(), capture));
        state.status = Status::Recording;
        let register = !state.registered;
        state.registered = true;
        register
    };
    ACTIVE.store(true, Ordering::Release);
    // Never call GStreamer while holding STATE: enabling categories may itself emit logs.
    unsafe {
        if register {
            gst_debug_add_log_function(Some(record), std::ptr::null_mut(), None);
        }
        // NULL names GStreamer's default logger (gstinfo.h + libpf PlayerFactory::create).
        // Our non-default callback remains installed. Do not duplicate trace output to stderr.
        gst_debug_remove_log_function(None);
        gst_debug_set_threshold_from_string(SPEC.as_ptr(), 1);
        gst_debug_set_active(1);
    }
    plx_base::eventlog::log("playback diagnostics: recording locally for at most 60s / 8MiB");
    Status::Recording
}

fn stop() {
    ACTIVE.store(false, Ordering::Release);
    let disable = {
        let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
        state.invalidate();
        let capture = state.capture.take();
        if state.status == Status::Saving {
            state.status = Status::Saved;
        }
        if capture.is_some() && state.status == Status::Recording {
            state.status = Status::Saved;
        }
        capture
    };
    if let Some((start, capture)) = disable {
        unsafe {
            gst_debug_set_active(0);
        }
        let reason = if capture.bytes == capture.limit {
            "size limit"
        } else if start.elapsed() >= capture.deadline {
            "time limit"
        } else if capture.active {
            "viewer"
        } else {
            "write failure"
        };
        plx_base::eventlog::log(&format!(
            "playback diagnostics: stopped reason={reason} bytes={}",
            capture.bytes
        ));
    }
}

/// Retire pending and active work before the application's shared privacy-erasure sweep.
/// This does not unlink files; the sweep owns deletion and its failure reporting.
pub fn retire() {
    let _io = EXPORT_IO.lock().unwrap_or_else(|e| e.into_inner());
    stop();
    STATE.lock().unwrap_or_else(|e| e.into_inner()).status = Status::Off;
}

/// App runner heartbeat: stop even when the firmware produces no further messages.
/// A stopped recorder performs no IO and loads no native library.
pub fn tick() {
    if !ACTIVE.load(Ordering::Acquire) {
        return;
    }
    let expire = {
        let state = STATE.lock().unwrap_or_else(|e| e.into_inner());
        state
            .capture
            .as_ref()
            .is_some_and(|(start, capture)| !capture.active || start.elapsed() >= capture.deadline)
    };
    if expire {
        stop();
    }
}

unsafe fn text(ptr: *const libc::c_char, limit: usize) -> String {
    if ptr.is_null() {
        return String::new();
    }
    String::from_utf8_lossy(std::slice::from_raw_parts(
        ptr.cast::<u8>(),
        libc::strnlen(ptr, limit),
    ))
    .into_owned()
}
std::thread_local! { static IN_CALLBACK: std::cell::Cell<bool> = const { std::cell::Cell::new(false) }; }
struct CallbackGuard;
impl Drop for CallbackGuard {
    fn drop(&mut self) {
        let _ = IN_CALLBACK.try_with(|flag| flag.set(false));
    }
}
unsafe extern "C" fn record(
    category: *mut c_void,
    level: i32,
    _file: *const libc::c_char,
    _function: *const libc::c_char,
    line: i32,
    _object: *mut c_void,
    message: *mut c_void,
    _data: *mut c_void,
) {
    if !ACTIVE.load(Ordering::Acquire) {
        return;
    }
    if IN_CALLBACK
        .try_with(|flag| flag.replace(true))
        .unwrap_or(true)
    {
        return;
    }
    let _guard = CallbackGuard;
    // Check admission before formatting, then release the lock before asking GStreamer to format
    // a message: an object formatter can itself log. The thread-local guard rejects recursion.
    let generation = {
        let state = STATE.lock().unwrap_or_else(|e| e.into_inner());
        if !state
            .capture
            .as_ref()
            .is_some_and(|(start, cap)| cap.active && start.elapsed() < cap.deadline)
        {
            return;
        }
        state.generation
    };
    let category = text(gst_debug_category_get_name(category), 64);
    let message = text(gst_debug_message_get(message), LINE_BYTE_LIMIT);
    let message = plx_base::eventlog::scrub::scrub_local(&message);
    let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
    if state.generation != generation {
        return;
    }
    let Some((start, capture)) = &mut state.capture else {
        return;
    };
    let elapsed = start.elapsed();
    if elapsed >= capture.deadline {
        capture.active = false;
        return;
    }
    if !capture.active {
        return;
    }
    let line = format!(
        "{}ms {category} {level}:{line} {message}\n",
        elapsed.as_millis()
    );
    if capture.push(elapsed, line.as_bytes()).is_err() {
        capture.active = false;
        state.status = Status::Failed;
    }
}

/// Capture a save transaction before submitting it to the shared off-frame executor.
/// Stop/Delete/Start invalidate its generation, including a queued save not yet begun.
pub fn prepare_export() -> u64 {
    let _io = EXPORT_IO.lock().unwrap_or_else(|e| e.into_inner());
    stop();
    let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
    state.status = Status::Saving;
    state.generation
}
/// Execute on the storage worker. IO serialization makes privacy deletion wait for an active
/// save, then invalidate queued work, before it unlinks the output. Nothing can recreate it later.
pub fn save_export(generation: u64) -> Status {
    let _io = EXPORT_IO.lock().unwrap_or_else(|e| e.into_inner());
    let (current, previous) = {
        let state = STATE.lock().unwrap_or_else(|e| e.into_inner());
        (state.generation, state.status)
    };
    let Some(status) = export_if_current(generation, current, export) else {
        return previous;
    };
    STATE.lock().unwrap_or_else(|e| e.into_inner()).status = status;
    status
}

fn export_if_current(
    generation: u64,
    current: u64,
    save: impl FnOnce() -> std::io::Result<()>,
) -> Option<Status> {
    if generation != current {
        return None;
    }
    Some(if save().is_ok() {
        Status::Exported
    } else {
        Status::Failed
    })
}

fn export_path() -> std::path::PathBuf {
    plx_base::paths::in_runtime_dir(plx_base::paths::runtime_file::PLAYBACK_DIAGNOSTICS)
}

/// The same no-follow/owner/single-link rule as the shared private log writer. Nonblocking open
/// prevents a pre-created FIFO from parking the UI before the descriptor can be validated.
fn open_owned_log(path: &std::path::Path) -> std::io::Result<File> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() || meta.uid() != unsafe { libc::geteuid() } || meta.nlink() != 1 {
        return Err(std::io::ErrorKind::PermissionDenied.into());
    }
    Ok(file)
}

/// Explicit local export. Readers are bounded even if an older developer build left a large file.
fn export() -> std::io::Result<()> {
    let trace = open_owned_log(&path())?;
    let mut output = plx_base::eventlog::open_log_append(&export_path())?;
    output.set_len(0)?;
    let events = open_owned_log(&plx_base::eventlog::events_log())
        .ok()
        .map(|mut events| {
            let length = events.metadata()?.len();
            events.seek(SeekFrom::Start(length.saturating_sub(256 * 1024)))?;
            Ok::<File, std::io::Error>(events)
        })
        .transpose()?;
    write_bundle(trace, events, &mut output)
}
fn write_bundle(
    trace: impl Read,
    events: Option<impl Read>,
    mut output: impl Write,
) -> std::io::Result<()> {
    output.write_all(b"=== Playback trace (up to 8 MiB) ===\n")?;
    std::io::copy(&mut trace.take(TRACE_BYTE_LIMIT as u64), &mut output)?;
    if let Some(events) = events {
        output.write_all(b"\n=== Recent app events (tail, up to 256 KiB) ===\n")?;
        std::io::copy(&mut events.take(256 * 1024), &mut output)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_save_queued_before_privacy_deletion_never_recreates_the_file() {
        let mut state = State::default();
        let queued = state.generation;
        state.invalidate(); // Stop/Delete use this same retirement before erasing local files.
        assert_eq!(
            export_if_current(queued, state.generation, || panic!(
                "stale save reached disk IO"
            )),
            None
        );
        assert_eq!(state.status, Status::Off);
    }
    #[test]
    fn an_unrequested_capture_is_off() {
        assert_eq!(State::default().status, Status::Off);
        assert!(State::default().capture.is_none());
    }
    #[test]
    fn the_live_writer_never_exceeds_its_byte_limit_even_for_a_large_record() {
        let mut capture = Capture::new(Vec::new(), 7, Duration::from_secs(60));
        capture.push(Duration::ZERO, b"abc").unwrap();
        capture.push(Duration::from_secs(1), b"defghijkl").unwrap();
        capture.push(Duration::from_secs(2), b"more").unwrap();
        assert_eq!(capture.writer, b"abcdefg");
        assert!(!capture.active);
    }
    #[test]
    fn export_refuses_symlink_and_hardlink_sources() {
        let root = std::env::temp_dir().join(format!(
            "plx-trace-reader-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        struct Remove(std::path::PathBuf);
        impl Drop for Remove {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _remove = Remove(root.clone());
        let source = root.join("source");
        std::fs::write(&source, b"fixture").unwrap();
        assert!(open_owned_log(&source).is_ok());
        let link = root.join("link");
        std::os::unix::fs::symlink(&source, &link).unwrap();
        assert!(open_owned_log(&link).is_err());
        std::fs::hard_link(&source, root.join("hardlink")).unwrap();
        assert!(open_owned_log(&source).is_err());
    }
    #[test]
    fn exporting_an_unbounded_old_trace_keeps_both_inputs_bounded() {
        let mut output = Vec::new();
        write_bundle(
            std::io::repeat(b'T'),
            Some(std::io::repeat(b'E')),
            &mut output,
        )
        .unwrap();
        assert_eq!(
            output.iter().filter(|&&b| b == b'T').count(),
            TRACE_BYTE_LIMIT
        );
        assert_eq!(output.iter().filter(|&&b| b == b'E').count(), 256 * 1024);
        assert!(output.len() < 9 * 1024 * 1024);
    }
    #[test]
    fn an_io_failure_closes_the_capture_guard() {
        struct Refused;
        impl Write for Refused {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::ErrorKind::WriteZero.into())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut capture = Capture::new(Refused, 100, Duration::from_secs(60));
        assert!(capture.push(Duration::ZERO, b"record").is_err());
        assert!(!capture.active);
    }
    #[test]
    fn no_record_at_or_after_the_deadline_reaches_the_file() {
        let mut capture = Capture::new(Vec::new(), 100, Duration::from_secs(60));
        capture.push(Duration::from_secs(59), b"before").unwrap();
        capture.push(Duration::from_secs(60), b"after").unwrap();
        assert_eq!(capture.writer, b"before");
        assert!(!capture.active);
    }
}
