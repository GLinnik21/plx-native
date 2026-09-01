//! `plxnative-sim` — the desktop UI simulator.
//!
//! The same application core the television runs, linked against a desktop SDL2 and desktop GL
//! instead of LG's. It draws the real interface, against a real Plex Media Server, on a laptop.
//!
//! **What it is for.** The television serializes the entire dev loop: one set, one app instance,
//! and two `tests/run.py` jobs kill each other's app. That makes every UI change a queue. Several
//! simulators can run at once — each with its own instance root (`PLXNATIVE_RUNTIME_DIR`), so each
//! has its own trigger namespace, its own remote FIFO and its own event log — which is what lets
//! independent UI and data-layer work proceed in parallel.
//!
//! **What it is NOT.** There is no video. The 29-symbol media seam does not exist off-device, and
//! `player::ffi`'s host arm reports the same "no video path" failure a television with no usable
//! ACB binding reports, so Play lands on the app's real failure read-out. Nor is it a substitute
//! for the device on anything the GPU decides: the `--fps` gates are calibrated to the SM9000's
//! Mali, and text rasterization here goes through a different FreeType. A green run on this binary
//! is evidence about a Mac. Layout, focus, navigation, and every byte of the Plex data layer are
//! the parts that do transfer — and they are most of the UI work.
//!
//! Usage:
//!   plxnative-sim [pms-host] [pms-port]
//! Environment:
//!   PLXNATIVE_PMS_HOST / PLXNATIVE_PMS_PORT   server to talk to (argv wins)
//!   PLXNATIVE_RUNTIME_DIR                     this instance's trigger/FIFO/log root
//!   PLXNATIVE_APP_DIR                         where appfont*.ttf and the icons live (repo `pkg/`)

use plxnative_modules::plex_run;
use std::ffi::CString;
use std::os::raw::c_int;

/// Defaults chosen to fail loudly rather than silently talk to the wrong thing.
const DEFAULT_PORT: u16 = 32400;

fn main() {
    // SIGPIPE needs no handling here, unlike `src/main.c:105`: that file installs `SIG_IGN` by hand
    // precisely because a C `main` skips Rust's `std::rt::init`. This IS a Rust main, so std has
    // already done it, and the first PMS socket closed mid-write cannot kill the process.

    let mut args = std::env::args().skip(1);
    let host = args
        .next()
        .or_else(|| std::env::var("PLXNATIVE_PMS_HOST").ok())
        .unwrap_or_default();
    let port: u16 = args
        .next()
        .or_else(|| std::env::var("PLXNATIVE_PMS_PORT").ok())
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_PORT);

    // No host is not an error — it is what a person who was SENT this app has. The boot host is
    // only ever consulted by the two paths that already hold credentials (the stored session and
    // the harness's injected token); with neither, `plex_run` goes to QR sign-in and plex.tv's
    // `/api/v2/resources` supplies the address, exactly as on a television out of the box.
    //
    // The dev loop keeps its loud failure where it belongs: the Makefile's `SIM_PRE` refuses to
    // launch when `SIM_PMS` is empty, so `make sim-run` still says "no PMS host" rather than
    // silently booting a sign-in screen at somebody expecting Home.
    if host.is_empty() {
        eprintln!(
            "plxnative-sim: no PMS host given — starting the plex.tv sign-in flow.\n\
             (pass one as `plxnative-sim <numeric-ip> [port]`, or set PLXNATIVE_PMS_HOST, to boot\n\
             straight at a known server. It must be a NUMERIC IP: stream.rs speaks HTTP/1.1 over a\n\
             raw socket with no DNS resolver, on the desktop exactly as on the television.)"
        );
    }

    // The instance root must exist before anything writes into it, and the event log is truncated
    // per launch — `src/main.c` does the same on the television, and `tests/run.py` relies on the
    // log starting empty to date its first line.
    let root = plxnative_modules::sim_runtime_dir();
    if let Err(e) = std::fs::create_dir_all(&root) {
        eprintln!(
            "plxnative-sim: cannot create runtime dir {}: {e}",
            root.display()
        );
        std::process::exit(1);
    }
    let events = plxnative_modules::sim_events_log();
    // The banner is the FIRST line of the log, so a log read from the middle, tailed, or pasted
    // into an issue still declares what produced it. Every heartbeat additionally carries `sim=1`
    // (see `app.rs`'s SIM_TAG) — one marker at the top is easy to scroll past.
    let banner = "sim: THIS IS THE DESKTOP SIMULATOR, not a television. No video pipeline; \
                  frame rates here describe this Mac's GPU and must never be read as an fps gate.\n";
    if let Err(e) = std::fs::write(&events, banner) {
        eprintln!("plxnative-sim: cannot truncate {}: {e}", events.display());
        std::process::exit(1);
    }

    eprintln!(
        "plxnative-sim: pms={host}:{port} runtime={} log={}",
        root.display(),
        events.display()
    );

    let c_host = CString::new(host).unwrap_or_else(|_| {
        eprintln!("plxnative-sim: host contains a NUL byte");
        std::process::exit(2)
    });

    // `as c_int` directly — an intermediate i16 would wrap every port above 32767.
    let rc = plex_run(c_host.as_ptr(), port as c_int);
    std::process::exit(rc);
}
