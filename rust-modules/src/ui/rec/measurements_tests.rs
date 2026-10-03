//! `Measurements` against a native source that memoizes its own widths and fitted lines — the
//! shape `text::TtfMeasure` has. This test lived in `text.rs`'s `fitted_line_tests`; it grades the
//! recorder's measurement capability, which `text` (the `gfx` layer) may not name, so it moved here
//! with the type it tests (module-layers step L5). It builds its stand-in from `text`'s two memos.
use super::Measurements;
use crate::text::{FittedLines, MeasuredBounds};
use crate::ui::machine::Measure;
use std::cell::Cell;
use std::collections::HashMap;
use std::ffi::CStr;
use std::os::raw::c_int;
use std::rc::Rc;

/// Counts every native width query (the stand-in for a live font).
#[derive(Default)]
struct CountMeasure(Cell<usize>);
impl Measure for CountMeasure {
    fn width(&self, s: &CStr, sz: i32, _: bool) -> f32 {
        self.0.set(self.0.get() + 1);
        s.to_string_lossy().chars().count() as f32 * sz as f32
    }
    fn cap_h(&self, _: i32) -> f32 { 1.0 }
    fn line_h(&self, _: i32) -> f32 { 1.0 }
}

#[test]
fn recording_and_replay_cannot_reuse_a_live_fitted_line() {
    struct NativeMemo(CountMeasure, std::cell::RefCell<FittedLines>,
        std::cell::RefCell<MeasuredBounds>);
    impl Measure for NativeMemo {
        fn width(&self, s: &CStr, sz: i32, bold: bool) -> f32 {
            self.2.borrow_mut().width(s, sz, bold as c_int, || (self.0.width(s, sz, bold), true))
        }
        fn cap_h(&self, sz: i32) -> f32 { self.0.cap_h(sz) }
        fn line_h(&self, sz: i32) -> f32 { self.0.line_h(sz) }
        fn fit_line(&self, s: &str, budget: f32, sz: i32, bold: bool) -> Rc<CStr> {
            self.1.borrow_mut().fit(self, s, budget, sz, bold)
        }
    }
    let source = Box::leak(Box::new(NativeMemo(CountMeasure::default(), Default::default(), Default::default())));
    let live = Measurements::Live(source);
    let warm = live.fit_line("episode", 20.0, 1, false);
    assert!(Rc::ptr_eq(&warm, &live.fit_line("episode", 20.0, 1, false)));
    let record = Measurements::record(source);
    let native_calls = source.0.0.get();
    assert_eq!(warm, record.fit_line("episode", 20.0, 1, false));
    assert_eq!(source.0.0.get(), native_calls, "recording queries must still use native cached metrics");
    let metrics = record.drain().unwrap().into_iter().collect::<HashMap<_, _>>();
    assert!(!metrics.is_empty(), "warm native memo must not hide recorded measurements");
    let mut replay = Measurements::Pending(Cell::new(false));
    replay.prepare(Some(&metrics));
    assert_eq!(warm, replay.fit_line("episode", 20.0, 1, false));
    assert!(replay.drain().is_ok());
    let mut missing = Measurements::Pending(Cell::new(false));
    missing.prepare(Some(&HashMap::new()));
    missing.fit_line("episode", 20.0, 1, false);
    assert!(missing.drain().is_err(), "warm native memo must not mask a replay miss");
}
