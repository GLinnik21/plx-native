//! Developer main-thread checker. The observer publishes data; only the frame thread paints.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

pub(super) const HANG_FATAL_MS: u64 = 2000;
const LINGER_MS: u64 = 3000;
#[derive(Clone, Copy)]
pub(super) enum Issue {
    Guard,
    /// `gpu_phase`: the stall was sampled inside a `frame draw` / `gl present` scope.
    Hang { ms: u64, gpu_phase: bool },
}

/// What a stall is allowed to prove on this boot. `log_only` is the `guard=log` escape hatch;
/// `software_gl` says the GL context rasterizes on the CPU, so GPU-phase time is not evidence.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Policy {
    pub(super) log_only: bool,
    pub(super) software_gl: bool,
}

/// A blocking-call guard is fatal on every developer boot: a software renderer changes how long
/// a frame takes, never whether a synchronous call belongs on the main thread. A two-second
/// stall is fatal too, EXCEPT inside a GPU phase on a software renderer — there the time is the
/// CPU rasterizer's (the CI simulator's first frames on Apple Software Renderer / llvmpipe),
/// which says nothing about the television's Mali. A stall outside a GPU phase stays fatal.
pub(super) fn fatal(dev: bool, policy: Policy, issue: Issue) -> bool {
    dev && !policy.log_only && match issue {
        Issue::Guard => true,
        Issue::Hang { ms, gpu_phase } => ms >= HANG_FATAL_MS && !(gpu_phase && policy.software_gl),
    }
}

/// Is this `GL_RENDERER` string a CPU rasterizer? Matched case-insensitively on the names the
/// desktop drivers report: Apple Software Renderer (macOS), Mesa llvmpipe / softpipe / swrast
/// ("Software Rasterizer"), SwiftShader, and WARP ("Microsoft Basic Render Driver", WSLg's D3D12
/// fallback). Anything else, the television's Mali included, is hardware.
pub(crate) fn software_renderer(renderer: &str) -> bool {
    const NAMES: &[&str] = &[
        "software renderer", "software rasterizer", "llvmpipe", "softpipe", "swiftshader",
        "basic render driver",
    ];
    let renderer = renderer.to_ascii_lowercase();
    NAMES.iter().any(|name| renderer.contains(name))
}

static SOFTWARE_GL: AtomicBool = AtomicBool::new(false);

/// Record the context's `GL_RENDERER` once at GL init. `None` (a null string) is unknown and
/// stays hardware: the watchdog must never become lenient because a string could not be read.
pub(crate) fn note_renderer(renderer: Option<&str>) {
    let software = renderer.is_some_and(software_renderer);
    SOFTWARE_GL.store(software, Ordering::Release);
    if software {
        crate::log("threadcheck: software GL renderer; frame draw/present stalls log without SIGABRT");
    }
}

/// The policy for the observer's next sample. The renderer flag is read per sample, not latched
/// with `log_only`, so the order of GL init and watchdog start cannot matter.
pub(super) fn policy(log_only: bool) -> Policy {
    Policy { log_only, software_gl: SOFTWARE_GL.load(Ordering::Acquire) }
}

/// A separate latch from the initial >250ms report: one request per continuous stall.
#[derive(Default)]
pub(super) struct KillLatch(bool);
impl KillLatch {
    pub(super) fn poll(&mut self, elapsed: Option<u64>, gpu_phase: bool, policy: Policy) -> bool {
        let Some(ms) = elapsed else { self.0 = false; return false };
        if !self.0 && fatal(true, policy, Issue::Hang { ms, gpu_phase }) {
            self.0 = true;
            return true;
        }
        false
    }
}

#[derive(Clone, Copy)]
pub(crate) struct Warning {
    pub(crate) kind: &'static str,
    pub(crate) ms: u64,
    pub(crate) label: &'static str,
    until: Option<u64>,
}
#[derive(Default)]
struct WarningState(Option<Warning>);
impl WarningState {
    fn reset(&mut self) { self.0 = None; }
    fn hang(&mut self, ms: u64, label: &'static str) {
        self.0 = Some(Warning { kind: "HANG", ms, label, until: None });
    }
    fn end(&mut self, now: u64, ms: u64) {
        if let Some(w) = &mut self.0 { w.ms = ms; w.until = Some(now.saturating_add(LINGER_MS)); }
    }
    fn visible(&self, now: u64) -> Option<Warning> {
        self.0.filter(|w| w.until.is_none_or(|until| now < until))
    }
}
static WARNING: Mutex<WarningState> = Mutex::new(WarningState(None));
fn now() -> u64 {
    static ORIGIN: OnceLock<Instant> = OnceLock::new();
    ORIGIN.get_or_init(Instant::now).elapsed().as_millis().min(u64::MAX as u128) as u64
}
pub(crate) fn warning() -> Option<Warning> {
    // Never wait behind an observer on the frame thread.
    WARNING.try_lock().ok().and_then(|s| s.visible(now()))
}
pub(super) fn hang(ms: u64, label: &'static str) {
    WARNING.lock().unwrap_or_else(|e| e.into_inner()).hang(ms, label);
}
pub(super) fn reset_warning() {
    WARNING.lock().unwrap_or_else(|e| e.into_inner()).reset();
}
pub(super) fn end(ms: u64) {
    WARNING.lock().unwrap_or_else(|e| e.into_inner()).end(now(), ms);
}
#[cfg(not(test))]
pub(super) fn guard(label: &'static str) {
    let mut s = WARNING.lock().unwrap_or_else(|e| e.into_inner());
    s.0 = Some(Warning { kind: "BLOCK", ms: 0, label, until: Some(now().saturating_add(LINGER_MS)) });
}

/// Publish a warning without starting the watchdog or sleeping; restore even after an assertion.
#[cfg(test)]
pub(crate) fn with_warning_for_test(f: impl FnOnce()) {
    crate::testlock::assert_held("runtime warning fixture");
    struct Restore(Option<Warning>);
    impl Drop for Restore {
        fn drop(&mut self) { WARNING.lock().unwrap_or_else(|e| e.into_inner()).0 = self.0; }
    }
    let _restore = Restore(WARNING.lock().unwrap_or_else(|e| e.into_inner()).0);
    hang(300, "warning fixture");
    f();
}

#[cfg(test)]
mod tests {
    use super::*;
    const HW: Policy = Policy { log_only: false, software_gl: false };
    const SW: Policy = Policy { log_only: false, software_gl: true };

    #[test]
    fn policy_matrix() {
        for dev in [false, true] {
            for log_only in [false, true] {
                for software_gl in [false, true] {
                    let policy = Policy { log_only, software_gl };
                    assert_eq!(fatal(dev, policy, Issue::Guard), dev && !log_only, "guard {policy:?}");
                    for gpu_phase in [false, true] {
                        for ms in [0, 250, 251, 1999, 2000, 2001, 5000] {
                            let want = dev && !log_only && ms >= 2000 && !(gpu_phase && software_gl);
                            assert_eq!(fatal(dev, policy, Issue::Hang { ms, gpu_phase }), want,
                                "{ms}ms gpu_phase={gpu_phase} {policy:?}");
                        }
                    }
                }
            }
        }
    }

    /// The CI simulator's abort (run 35866841350: `fatal: 2091ms in frame draw` on Apple
    /// Software Renderer): a software rasterizer's frame draw is not a hang, but a blocking call
    /// and a stall outside a GPU phase still are, and hardware GL keeps the two-second limit.
    #[test]
    fn a_software_renderer_draw_is_not_a_hang_but_a_guard_still_is() {
        assert!(!fatal(true, SW, Issue::Hang { ms: 2091, gpu_phase: true }));
        assert!(!fatal(true, SW, Issue::Hang { ms: 60_000, gpu_phase: true }));
        assert!(fatal(true, SW, Issue::Guard));
        assert!(fatal(true, SW, Issue::Hang { ms: 2000, gpu_phase: false }));
        assert!(fatal(true, HW, Issue::Hang { ms: 2000, gpu_phase: true }));
        assert!(fatal(true, HW, Issue::Guard));
    }

    #[test]
    fn renderer_strings_classify() {
        for software in [
            "Apple Software Renderer", "llvmpipe (LLVM 15.0.7, 256 bits)", "softpipe",
            "Software Rasterizer", "Google SwiftShader", "SwiftShader Device (Subzero)",
            "D3D12 (Microsoft Basic Render Driver)", "LLVMPIPE",
        ] {
            assert!(software_renderer(software), "{software}");
        }
        for hardware in [
            "Mali-T860", "Mali-G52", "Apple M1", "Apple M3 Pro", "AMD Radeon Pro 5500M OpenGL Engine",
            "Intel(R) Iris(TM) Plus Graphics 655", "D3D12 (NVIDIA GeForce RTX 3070)",
            "Mesa Intel(R) UHD Graphics 620 (KBL GT2)", "", "\u{fffd}\u{fffd}",
        ] {
            assert!(!software_renderer(hardware), "{hardware}");
        }
    }

    #[test]
    fn kill_request_is_once_per_stall_and_never_early() {
        let mut latch = KillLatch::default();
        for ms in [0, 250, 251, 1999] { assert!(!latch.poll(Some(ms), true, HW)); }
        assert!(latch.poll(Some(2000), true, HW));
        for ms in [2000, 2100, 5000] { assert!(!latch.poll(Some(ms), true, HW)); }
        assert!(!latch.poll(None, true, HW));
        assert!(latch.poll(Some(2000), true, HW));
        let mut escaped = KillLatch::default();
        let log = Policy { log_only: true, software_gl: false };
        for ms in [251, 1999, 2000, 5000] { assert!(!escaped.poll(Some(ms), false, log)); }
        let mut software = KillLatch::default();
        for ms in [251, 1999, 2000, 5000] { assert!(!software.poll(Some(ms), true, SW)); }
        assert!(software.poll(Some(5100), false, SW), "a stall outside a GPU phase on software GL is fatal");
    }
    #[test]
    fn warning_lives_through_hang_then_lingers() {
        let mut state = WarningState::default();
        assert!(state.visible(0).is_none());
        state.hang(300, "probe");
        assert_eq!(state.visible(10000).unwrap().label, "probe");
        state.end(10000, 1000);
        assert_eq!(state.visible(12999).unwrap().ms, 1000);
        assert!(state.visible(13000).is_none());
        state.hang(400, "next");
        assert_eq!(state.visible(14000).unwrap().label, "next");
    }

    #[test]
    fn a_pause_discards_the_warning_without_linger() {
        let mut state = WarningState::default();
        state.hang(300, "gl present");
        assert!(state.visible(300).is_some());
        state.reset();
        assert!(state.visible(30_300).is_none());
        state.hang(300, "frame draw");
        state.end(30_600, 400);
        state.reset();
        assert!(state.visible(30_601).is_none());
    }

}
