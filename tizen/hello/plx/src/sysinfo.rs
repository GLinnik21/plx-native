//! The environment lines the tester pastes back: architecture, kernel, glibc, CPU, process.
//!
//! Architecture is UNDETERMINED for Tizen 6-9 sets (the 32-bit ARMv7 softfp assumption is
//! research, not measurement; some sets may be aarch64), so everything that could tell is logged:
//! this library's own compile-time arch, the kernel's `uname` machine, the pointer width of the
//! process that loaded us, and the CPU lines of /proc/cpuinfo.

use crate::log::plog;
use std::ffi::{c_char, CStr};

fn field(f: &[c_char]) -> String {
    // SAFETY: utsname fields are NUL-terminated within their arrays.
    unsafe { CStr::from_ptr(f.as_ptr()) }.to_string_lossy().into_owned()
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn glibc_version() -> String {
    // SAFETY: returns a static NUL-terminated string.
    unsafe { CStr::from_ptr(libc::gnu_get_libc_version()) }
        .to_string_lossy()
        .into_owned()
}
#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
fn glibc_version() -> String {
    "n/a (not linux-gnu)".to_string()
}

/// Print the first `max` lines of `path` whose text starts with one of `prefixes` (all lines when
/// `prefixes` is empty). A missing file is a one-line note, not an error.
fn dump_file(label: &str, path: &str, prefixes: &[&str], max: usize) {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let mut n = 0;
            for l in text.lines() {
                if n >= max {
                    break;
                }
                if prefixes.is_empty() || prefixes.iter().any(|p| l.starts_with(p)) {
                    plog!("{label}: {}", l.trim());
                    n += 1;
                }
            }
            if n == 0 {
                plog!("{label}: {path} has no matching lines");
            }
        }
        Err(e) => plog!("{label}: cannot read {path}: {e}"),
    }
}

pub fn log_environment(data_dir: &str) {
    plog!(
        "plx-hello {} (step 1: host start / .so load / window clear / keys)",
        env!("CARGO_PKG_VERSION")
    );
    plog!("epoch {} (device clock; may be unset)", crate::log::epoch_secs());
    plog!(
        "arch(compile-time) {} / os {} / pointer-width {} bytes / endian {}",
        std::env::consts::ARCH,
        std::env::consts::OS,
        std::mem::size_of::<usize>(),
        if cfg!(target_endian = "little") { "little" } else { "big" }
    );

    // SAFETY: an all-zero utsname is a valid out-parameter.
    let mut u: libc::utsname = unsafe { std::mem::zeroed() };
    // SAFETY: `u` is a valid, writable utsname.
    if unsafe { libc::uname(&mut u) } == 0 {
        plog!(
            "uname machine={} sysname={} release={} version={}",
            field(&u.machine),
            field(&u.sysname),
            field(&u.release),
            field(&u.version)
        );
    } else {
        plog!("uname failed");
    }
    plog!("glibc {}", glibc_version());
    plog!("pid {} data_dir {data_dir}", std::process::id());
    match std::fs::read_link("/proc/self/exe") {
        Ok(p) => plog!("process exe {}", p.display()),
        Err(e) => plog!("process exe unreadable: {e}"),
    }
    dump_file(
        "cpuinfo",
        "/proc/cpuinfo",
        &["Processor", "model name", "CPU architecture", "Features", "flags", "Hardware"],
        8,
    );
    // Tizen's platform version, when the file exists (the host also logs it through Tizen.System).
    dump_file("tizen-release", "/etc/tizen-release", &[], 4);
    dump_file("os-release", "/etc/os-release", &["NAME", "VERSION", "PRETTY_NAME", "TIZEN"], 6);
}
