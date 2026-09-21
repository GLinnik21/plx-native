//! Main-thread checker read-out, drawn only by the frame thread, above every route.
use super::{consts::SAFE, label::Label, theme, Painter, Rect};
use std::{cell::Cell, ffi::CString};
thread_local! { static WAS_VISIBLE: Cell<bool> = const { Cell::new(false) }; }
pub(crate) fn update() {
    let visible = crate::task::runtime_check::warning().is_some();
    let previous = WAS_VISIBLE.with(|v| v.replace(visible));
    // Keep presenting through the linger and clear the final painted warning on expiry.
    if visible || previous { super::idle::invalidate(); }
}
pub(crate) fn draw() {
    let Some(w) = crate::task::runtime_check::warning() else { return };
    let r = Rect::new(SAFE.x, SAFE.y + SAFE.h - 80.0, SAFE.w, 80.0);
    let p = Painter::root();
    p.rect(r, theme::space::SM, theme::RUNTIME_WARNING, theme::RUNTIME_WARNING, 0.0);
    if let Ok(text) = CString::new(format!("MAIN THREAD {} {}ms · {}", w.kind, w.ms, w.label)) {
        Label::new(text.as_ptr(), theme::size::BODY, theme::TEXT_PRIMARY).bold().draw(
            p, Rect::new(r.x + theme::space::MD, r.y, r.w - 2.0 * theme::space::MD, r.h));
    }
}
