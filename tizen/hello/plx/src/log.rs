//! The log file: `<data_dir>/plx-hello.log`, append, one line per call, flushed per line.
//!
//! The C# host appends its own `[host]` lines to the same file, so a tester reads ONE file to see
//! the whole story. Every line is also mirrored to stderr (visible with `sdb dlog`-style tools on
//! an emulator).
//!
//! A log that cannot be opened is reported on stderr and is NOT fatal: the window and key tests are
//! still worth running, and the tester sees the colour change without the file.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::sync::{Mutex, OnceLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

struct Sink {
    file: Option<File>,
    t0: Instant,
}

static SINK: OnceLock<Mutex<Sink>> = OnceLock::new();

/// Mirror to stderr WITHOUT `eprint!`: that panics when the write fails (a closed stderr), and
/// logging also runs inside the panic hook, where a second panic aborts the process with no evidence.
pub fn to_stderr(text: &str) {
    let _ = std::io::stderr().write_all(text.as_bytes());
}

/// Open (append) `<dir>/plx-hello.log`. Idempotent; the first call wins. Returns the path and
/// whether the file opened.
pub fn init(dir: &str) -> (String, bool) {
    let path = format!("{}/plx-hello.log", dir.trim_end_matches('/'));
    let file = OpenOptions::new().create(true).append(true).open(&path);
    let ok = file.is_ok();
    if let Err(e) = &file {
        to_stderr(&format!("plx: cannot open log {path}: {e}\n"));
    }
    let _ = SINK.set(Mutex::new(Sink {
        file: file.ok(),
        t0: Instant::now(),
    }));
    (path, ok)
}

fn tid() -> i64 {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: gettid takes no arguments and cannot fail.
        unsafe { libc::syscall(libc::SYS_gettid) as i64 }
    }
    #[cfg(not(target_os = "linux"))]
    {
        0
    }
}

/// Write one line. Never panics, never blocks on anything but the file.
pub fn line(msg: &str) {
    let Some(m) = SINK.get() else {
        to_stderr(&format!("plx: {msg}\n"));
        return;
    };
    let mut s = m.lock().unwrap_or_else(|e| e.into_inner());
    let t = s.t0.elapsed();
    let text = format!("[plx +{:>4}.{:03}s t{}] {msg}\n", t.as_secs(), t.subsec_millis(), tid());
    if let Some(f) = s.file.as_mut() {
        let _ = f.write_all(text.as_bytes());
        let _ = f.flush();
    }
    to_stderr(&text);
}

/// Seconds since the Unix epoch, for the header line (the host's clock may be unset on a TV).
pub fn epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

macro_rules! plog {
    ($($arg:tt)*) => { $crate::log::line(&format!($($arg)*)) };
}
pub(crate) use plog;
