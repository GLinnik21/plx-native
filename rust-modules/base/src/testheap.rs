//! The process's live heap bytes, read from the C allocator, for tests that pin how many copies of
//! a buffer a path keeps (issue #1328: a file's font attachments, copied by FFmpeg, by the store
//! and by libass).
//!
//! A counting `#[global_allocator]` sees Rust's allocations only, and the copies that matter here
//! are made by FFmpeg and libass, which call `malloc` themselves. So the meter asks the platform's
//! malloc what it holds: `malloc_zone_statistics` on macOS (every zone, `size_in_use`) and
//! `mallinfo2` on glibc (`uordblks` + `hblkhd`: the arenas and the mmapped chunks). It is the bytes
//! the program has asked for and not freed, not the resident set: the same number on every host
//! whatever the pager does, which is what a copy count needs. A platform with neither reports
//! `None`, and a test that needs the meter says it could not run rather than passing.
//!
//! A test measuring a path must not share its process with other tests (a parallel test's
//! allocations are in the number): [`in_fresh_process`] runs the test binary again with only that
//! test selected, and the child reports its numbers with [`report`]. The pattern, in the test:
//!
//! ```ignore
//! let numbers = if testheap::is_child() { measure_and_report(); return; }
//!               else { testheap::in_fresh_process("module::the_test") };
//! assert!(numbers["peak"] < budget);
//! ```

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// The environment variable a fresh process is started with, naming the test it runs.
const CHILD_ENV: &str = "PLX_TESTHEAP_CHILD";
/// The prefix of a line [`report`] prints and [`in_fresh_process`] reads.
const LINE_PREFIX: &str = "TESTHEAP ";

/// Bytes the C allocator holds live across the whole process, or `None` on a platform whose
/// allocator cannot be asked.
pub fn live_bytes() -> Option<usize> {
    #[cfg(target_os = "macos")]
    {
        // SAFETY: a zeroed statistics struct is valid, and a null zone means "every zone".
        let mut stats: libc::malloc_statistics_t = unsafe { std::mem::zeroed() };
        unsafe { libc::malloc_zone_statistics(std::ptr::null_mut(), &mut stats) };
        Some(stats.size_in_use)
    }
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        // SAFETY: `mallinfo2` takes no argument and returns a plain struct.
        let info = unsafe { libc::mallinfo2() };
        Some(info.uordblks + info.hblkhd)
    }
    #[cfg(not(any(target_os = "macos", all(target_os = "linux", target_env = "gnu"))))]
    {
        None
    }
}

/// The most live bytes seen while `work` ran, sampled every 100 microseconds from a thread of its
/// own, with the work's result. A buffer that lives for less than a sample can be missed, so the
/// value is a floor on the true peak; the copies this exists to count live for seconds.
pub fn peak_during<R>(work: impl FnOnce() -> R) -> (R, usize) {
    let stop = Arc::new(AtomicBool::new(false));
    let peak = Arc::new(AtomicUsize::new(live_bytes().unwrap_or(0)));
    let sampler = {
        let (stop, peak) = (Arc::clone(&stop), Arc::clone(&peak));
        crate::task::spawn("testheap-sampler", move || {
            while !stop.load(Ordering::Acquire) {
                peak.fetch_max(live_bytes().unwrap_or(0), Ordering::Relaxed);
                std::thread::sleep(Duration::from_micros(100));
            }
            peak.fetch_max(live_bytes().unwrap_or(0), Ordering::Relaxed);
        })
        .expect("the heap sampler thread starts")
    };
    let result = work();
    stop.store(true, Ordering::Release);
    sampler.join().expect("the heap sampler does not panic");
    (result, peak.load(Ordering::Relaxed))
}

/// Is this process the fresh one [`in_fresh_process`] started?
pub fn is_child() -> bool {
    std::env::var_os(CHILD_ENV).is_some()
}

/// Print one number for the parent test to read. Only meaningful in a child.
pub fn report(key: &str, value: u64) {
    println!("{LINE_PREFIX}{key}={value}");
}

/// Run `test` (the full path libtest lists, e.g. `player::ass::tests::name`) alone in a fresh
/// process of this same test binary, `--ignored` included so an ignored test can use it, and
/// return the numbers it [`report`]ed. Panics, with the child's output, when the child fails.
/// The child inherits this process's environment (the libraries a host test finds by
/// `PLXNATIVE_APP_DIR` or `PLX_FFMPEG_DIR`), plus `PLX_TESTHEAP_CHILD`.
pub fn in_fresh_process(test: &str) -> HashMap<String, u64> {
    in_fresh_process_with_env(test, &[])
}

/// [`in_fresh_process`] with more environment variables for the child. A test that measures what a
/// path allocates hands its input over this way, made by the parent: whatever the child allocated to
/// MAKE the input would otherwise be in its heap, and on macOS a freed large block that malloc keeps
/// cached is handed back, rounded up, to the next large allocation, so the number moves with it.
pub fn in_fresh_process_with_env(test: &str, env: &[(&str, &std::ffi::OsStr)]) -> HashMap<String, u64> {
    let exe = std::env::current_exe().expect("the test binary has a path");
    let out = std::process::Command::new(exe)
        .args(["--exact", test, "--include-ignored", "--test-threads=1", "--nocapture"])
        .env(CHILD_ENV, test)
        .envs(env.iter().copied())
        .output()
        .expect("the test binary starts again");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "the fresh process for {test} failed ({:?})\n--- stdout\n{text}\n--- stderr\n{}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let numbers: HashMap<String, u64> = text
        .lines()
        // `--nocapture` puts libtest's own "test name ... " in front of the first report.
        .filter_map(|line| line.find(LINE_PREFIX).map(|at| &line[at + LINE_PREFIX.len()..]))
        .filter_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            Some((key.to_owned(), value.trim().parse().ok()?))
        })
        .collect();
    assert!(!numbers.is_empty(), "the fresh process for {test} reported nothing\n--- stdout\n{text}");
    numbers
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The meter sees a C allocation and a Rust one, and sees them go.
    #[test]
    fn the_meter_counts_what_is_held_and_forgets_what_is_freed() {
        let Some(before) = live_bytes() else { return };
        let held = vec![1u8; 64 << 20];
        let with = live_bytes().unwrap();
        assert!(with >= before + (60 << 20), "a 64 MiB vector is held: {before} -> {with}");
        drop(held);
        let (_, peak) = peak_during(|| {
            let transient = vec![2u8; 32 << 20];
            std::thread::sleep(Duration::from_millis(20));
            std::hint::black_box(&transient);
        });
        assert!(peak >= before + (30 << 20), "the sampler saw a 32 MiB transient: {before} -> {peak}");
    }
}
