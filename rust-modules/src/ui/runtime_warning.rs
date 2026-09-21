//! Main-thread checker read-out, drawn only by the frame thread, above every route.
use super::{consts::SAFE, label::Label, theme, Painter, Rect};
use std::{cell::Cell, ffi::CString};
thread_local! { static WAS_VISIBLE: Cell<bool> = const { Cell::new(false) }; }
pub(crate) fn update(controlled: bool) {
    // The app supplies its bootstrap predicate. Wall-clock diagnostics must not affect a
    // controlled run's per-frame present grade; checker logging and fatal policy stay active.
    if controlled {
        WAS_VISIBLE.with(|v| v.set(false));
        return;
    }
    let visible = crate::task::runtime_check::warning().is_some();
    let previous = WAS_VISIBLE.with(|v| v.replace(visible));
    // Keep presenting through the linger and clear the final painted warning on expiry.
    if visible || previous { super::idle::invalidate(); }
}
pub(crate) fn draw(controlled: bool) {
    draw_with(controlled, paint);
}
fn draw_with(controlled: bool, paint: impl FnOnce(crate::task::runtime_check::Warning)) {
    if controlled { return; }
    if let Some(w) = crate::task::runtime_check::warning() { paint(w); }
}
fn paint(w: crate::task::runtime_check::Warning) {
    let r = Rect::new(SAFE.x, SAFE.y + SAFE.h - 80.0, SAFE.w, 80.0);
    let p = Painter::root();
    p.rect(r, theme::space::SM, theme::RUNTIME_WARNING, theme::RUNTIME_WARNING, 0.0);
    if let Ok(text) = CString::new(format!("MAIN THREAD {} {}ms · {}", w.kind, w.ms, w.label)) {
        Label::new(text.as_ptr(), theme::size::BODY, theme::TEXT_PRIMARY).bold().draw(
            p, Rect::new(r.x + theme::space::MD, r.y, r.w - 2.0 * theme::space::MD, r.h));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_controlled_boot_warning_neither_invalidates_nor_draws() {
        let _serial = crate::testlock::serial();
        crate::task::runtime_check::with_warning_for_test(|| {
            super::super::idle::reset_for_test();
            WAS_VISIBLE.with(|v| v.set(false));
            update(true);
            let damage = super::super::idle::take_local_damage();
            let mut paints = 0;
            draw_with(true, |_| paints += 1);
            assert_eq!((damage, paints), (0, 0), "controlled boots must exclude real-time warning presentation");
            assert!(crate::task::runtime_check::warning().is_some(), "presentation must not disable the checker");
        });
    }
    #[test]
    fn live_warnings_still_present_and_controlled_mode_clears_no_pixels() {
        let _serial = crate::testlock::serial();
        crate::task::runtime_check::with_warning_for_test(|| {
            super::super::idle::reset_for_test();
            WAS_VISIBLE.with(|v| v.set(false));
            update(false);
            assert!(super::super::idle::take_local_damage() > 0);
            let mut paints = 0;
            draw_with(false, |_| paints += 1);
            assert_eq!(paints, 1);
            update(true);
            draw_with(true, |_| paints += 1);
            assert_eq!(super::super::idle::take_local_damage(), 0, "even a stale visible flag must not force a controlled present");
            assert_eq!(paints, 1);
        });
    }

}
