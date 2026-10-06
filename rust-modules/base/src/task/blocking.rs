//! The frame thread must not wait on storage or synchronous platform calls. Boot runs outside
//! the scope; the application loop and dispatcher enter it, including when driven by host tests.

use std::{cell::Cell, marker::PhantomData, panic::Location, rc::Rc, time::Instant};

/// An immutable static descriptor lets the watchdog publish a whole label with one pointer.
/// `const { &BlockingLabel::new("literal") }` is evaluated into static storage; no scope
/// allocates or interns strings.
pub struct BlockingLabel {
    pub(super) text: &'static str,
}
impl BlockingLabel {
    pub const fn new(text: &'static str) -> Self { Self { text } }
    pub fn text(&self) -> &'static str { self.text }
}

thread_local! {
    static FRAMES: Cell<u32> = const { Cell::new(0) };
    static ALLOWED: Cell<u32> = const { Cell::new(0) };
}

pub struct FrameScope(PhantomData<Rc<()>>);
impl FrameScope {
    pub fn enter() -> Self {
        FRAMES.with(|depth| depth.set(depth.get() + 1));
        Self(PhantomData)
    }
}
impl Drop for FrameScope {
    fn drop(&mut self) { FRAMES.with(|depth| depth.set(depth.get() - 1)); }
}

/// The exception a frame-thread scope is currently running under: its label and the call site
/// that opened it. Both are `'static` (the label is a descriptor in static storage and
/// `Location::caller()` is a compiler-emitted static), so reading or restoring one is a copy of
/// two pointers and never allocates. The watchdog's published pointer is unchanged: it still
/// names only the innermost label.
type Exception = (&'static BlockingLabel, &'static Location<'static>);

thread_local! {
    static EXCEPTION: Cell<Option<Exception>> = const { Cell::new(None) };
}

pub struct AllowBlocking {
    _label: super::watchdog::LabelScope,
    /// Where the exception was opened (`#[track_caller]`).
    at: &'static Location<'static>,
    /// The exception this one shadows, restored on drop.
    previous: Option<Exception>,
}
/// Explicit exceptions belong at user actions, with a reason and follow-up at the call site. The
/// call site is recorded (`#[track_caller]`) and appears in every `main-thread block:` line the
/// exception covers (`under <label> at <file:line>`), so wrap a helper that hands out exceptions
/// in `#[track_caller]` too, or every use reports the helper.
///
/// Every non-test use is also listed in `ci/allow/blocking.txt`, which `ci/check-deps.sh`'s
/// `blocking` gate holds to an exact set: a new `allow_blocking(` fails CI, and so does a stale
/// entry, so the list can only shrink. No route-changing PMS call is left on the frame thread: every
/// rebase, recovery and resume is a flight whose PMS half runs on a worker (`media::route::flight`),
/// and the menu-play season load is the season tabs' async `load_season`.
///
/// The ledger's final two entries are both refused-worker fallbacks, not user actions:
/// `task::spawn_or_inline` (the OS refused a worker thread, so server-side stop work that must not
/// be dropped runs inline under the caller's label) and `route::decision::scrobble_stop` (a refused
/// `spawn_small_keeping` — its old reporter handle and stop fence must stay ordered, which a plain
/// inline cannot promise).
#[track_caller]
pub fn allow_blocking(reason: &'static BlockingLabel) -> AllowBlocking {
    assert!(!reason.text.is_empty());
    let at = Location::caller();
    ALLOWED.with(|depth| depth.set(depth.get() + 1));
    let previous = EXCEPTION.with(|held| held.replace(Some((reason, at))));
    AllowBlocking { _label: super::watchdog::enter_label(reason), at, previous }
}
impl AllowBlocking {
    /// Where this exception was opened.
    pub fn site(&self) -> &'static Location<'static> { self.at }
}
impl Drop for AllowBlocking {
    fn drop(&mut self) {
        ALLOWED.with(|depth| depth.set(depth.get() - 1));
        EXCEPTION.with(|held| held.set(self.previous));
    }
}

/// Whether this thread is inside a [`FrameScope`] (the frame thread).
pub(super) fn in_frame() -> bool { FRAMES.with(|depth| depth.get() != 0) }

#[must_use = "keep the guard for the duration of the blocking operation"]
pub struct BlockingGuard {
    label: &'static str,
    started: Option<Instant>,
    /// Where the guarded call was made (`#[track_caller]`).
    at: &'static Location<'static>,
    /// The exception this call ran under, if one was held.
    under: Option<Exception>,
    _label: super::watchdog::LabelScope,
    _thread: PhantomData<Rc<()>>,
}

/// Allocation-free by design: it runs before every guarded call, so it only copies pointers.
/// Formatting happens in [`BlockingGuard`]'s drop, which is already the slow branch.
#[track_caller]
pub fn assert_may_block(label: &'static BlockingLabel) -> BlockingGuard {
    let at = Location::caller();
    let in_frame = in_frame();
    if cfg!(any(test, feature = "test-support")) && in_frame && !ALLOWED.with(|depth| depth.get() != 0) {
        panic!("main-thread block: {} at {}", label.text, Site(at));
    }
    #[cfg(all(feature = "threadcheck", not(any(test, feature = "test-support"))))]
    if in_frame && !ALLOWED.with(|depth| depth.get() != 0) {
        if super::runtime_check::fatal(true, super::runtime_check::policy(crate::devtrig::guard_log_only()), super::runtime_check::Issue::Guard) {
            // log writes directly to an unbuffered File before aborting this thread.
            crate::eventlog::log(&format!("main-thread block: {} at {} (fatal; aborting)", label.text, Site(at)));
            std::process::abort();
        }
        super::runtime_check::guard(label.text);
    }
    BlockingGuard { label: label.text, started: in_frame.then(Instant::now), at,
        under: EXCEPTION.with(Cell::get),
        _label: super::watchdog::enter_label(label), _thread: PhantomData }
}

/// `file:line` — `Location`'s own `Display` also prints the column, which no one greps by.
struct Site<'a>(&'a Location<'a>);
impl std::fmt::Display for Site<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.0.file(), self.0.line())
    }
}

/// `main-thread block: <label> <ms>ms at <file:line>[ under <exception> at <file:line>]`.
fn report_line(label: &str, ms: u128, at: &Location<'_>, under: Option<Exception>) -> String {
    let mut line = format!("main-thread block: {label} {ms}ms at {}", Site(at));
    if let Some((exception, opened)) = under {
        line.push_str(&format!(" under {} at {}", exception.text, Site(opened)));
    }
    line
}

/// One report per (label, call site): a label shared by several callers (`PMS HTTP`) must still
/// name each caller once, which a label-only key hid behind whichever ran first.
fn first_report(label: &'static str, at: &'static Location<'static>) -> bool {
    static REPORTED: std::sync::Mutex<std::collections::BTreeSet<(&'static str, &'static Location<'static>)>> =
        std::sync::Mutex::new(std::collections::BTreeSet::new());
    REPORTED.lock().unwrap_or_else(|e| e.into_inner()).insert((label, at))
}

impl Drop for BlockingGuard {
    fn drop(&mut self) {
        if let Some(started) = self.started {
            if first_report(self.label, self.at) {
                crate::eventlog::log(&report_line(self.label, started.elapsed().as_millis(), self.at, self.under));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[track_caller]
    fn here() -> &'static Location<'static> { Location::caller() }

    #[test]
    fn nested_scopes_and_exceptions_restore_after_unwind() {
        let frame = FrameScope::enter();
        let _ = std::panic::catch_unwind(|| {
            let _nested = FrameScope::enter();
            let _allowed = allow_blocking(const { &BlockingLabel::new("test explicit user action") });
            let _call = assert_may_block(const { &BlockingLabel::new("allowed fixture") });
            panic!("unwind the scopes");
        });
        assert!(std::panic::catch_unwind(|| assert_may_block(const { &BlockingLabel::new("after exception") })).is_err());
        drop(frame);
        let _boot = assert_may_block(const { &BlockingLabel::new("boot outside frame") });
    }

    #[test]
    fn allow_blocking_records_its_call_site() {
        let line = line!() + 1;
        let held = allow_blocking(const { &BlockingLabel::new("site fixture") });
        assert_eq!(held.at.line(), line);
        assert!(held.at.file().ends_with("blocking.rs"), "{}", held.at);
    }

    #[test]
    fn a_guard_records_its_own_site_and_the_exception_it_ran_under() {
        let _frame = FrameScope::enter();
        let outer_line = line!() + 1;
        let _outer = allow_blocking(const { &BlockingLabel::new("outer exception") });
        let guard_line = line!() + 1;
        let guard = assert_may_block(const { &BlockingLabel::new("guarded call") });
        assert_eq!(guard.at.line(), guard_line);
        let (label, at) = guard.under.expect("the guard ran under an exception");
        assert_eq!(label.text, "outer exception");
        assert_eq!(at.line(), outer_line);
        // An inner exception shadows the outer one, and the outer one returns when it ends.
        {
            let _inner = allow_blocking(const { &BlockingLabel::new("inner exception") });
            let inner = assert_may_block(const { &BlockingLabel::new("inner call") });
            assert_eq!(inner.under.map(|(l, _)| l.text), Some("inner exception"));
        }
        let after = assert_may_block(const { &BlockingLabel::new("after inner") });
        assert_eq!(after.under.map(|(l, _)| l.text), Some("outer exception"));
    }

    #[test]
    fn the_report_line_names_the_call_site_and_the_exception() {
        let at = here();
        let plain = report_line("PMS HTTP", 312, at, None);
        assert!(plain.starts_with("main-thread block: PMS HTTP 312ms at "), "{plain}");
        assert!(plain.ends_with(&format!("blocking.rs:{}", at.line())), "{plain}");
        assert!(!plain.contains(" under "), "{plain}");
        let held = report_line("PMS HTTP", 312, at, Some((const { &BlockingLabel::new("route PMS call") }, at)));
        assert!(held.contains(" under route PMS call at "), "{held}");
    }

    #[test]
    fn a_label_is_reported_once_per_call_site() {
        fn same_site(label: &'static str) -> bool { first_report(label, here()) }
        // Two different sites for one label both report; the same site reports once.
        let a = first_report("dedup fixture", here());
        let b = first_report("dedup fixture", here());
        assert!(a && b, "distinct sites are distinct reports");
        assert!(same_site("dedup same-site fixture"));
        assert!(!same_site("dedup same-site fixture"), "the same (label, site) is reported once");
    }
}
