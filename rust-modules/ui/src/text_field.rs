//! `TextField` — one labelled line of text a person types with the television's keyboard.
//!
//! A field is a caption-sized LABEL over a PLATE that holds the value. The plate is the focus
//! stop: idle it is the ordinary control face ([`theme::CONTROL_IDLE_FILL`] under
//! [`theme::TEXT_PRIMARY`] ink), focused it is the table row's pill ([`theme::ACCENT`] under
//! [`theme::ACCENT_INK`]), so a form of fields reads as the Settings rows it sits beside. While the
//! keyboard is up for it the caret is drawn at the insertion point.
//!
//! What it shows is decided here, not by the caller:
//!
//! - an empty value shows the placeholder (if any) in the dimmed ink of its face;
//! - a SECRET value shows one [`SECRET_DOT`] per character, never the text — the draw never sees
//!   anything it could leak to a capture;
//! - a value wider than the plate scrolls so the caret stays in view, clipped to the plate. It
//!   never ends in an ellipsis: what a person typed is theirs, and the end of it is what they are
//!   looking at.
//!
//! Editing (the [`crate::text_buffer::TextBuffer`] behind the value, the keyboard and its events)
//! is the owning screen's; this is the drawing and the geometry.

use crate::label::{Label, VAlign};
use crate::screen::ClipScope;
use crate::{theme, Painter, Rect};
use plx_machine::machine::Measure;
use std::ffi::{CStr, CString};

/// The label band above the plate: one caption line.
pub const LABEL_H: f32 = 32.0;
/// Between the label band and the plate.
pub const LABEL_GAP: f32 = theme::space::XS;
/// The plate: one `BODY` line with the table row's air around it.
pub const PLATE_H: f32 = crate::table::ROW_H;
/// A whole field, label to plate bottom.
pub const FIELD_H: f32 = LABEL_H + LABEL_GAP + PLATE_H;
/// The value's inset from the plate's edges, the table's content inset.
pub const PLATE_PAD: f32 = crate::table::CONTENT_X;
/// What a secret value shows in place of each character.
pub const SECRET_DOT: char = '•';

const CARET_W: f32 = 3.0;
const VALUE_SIZE: std::os::raw::c_int = theme::size::BODY;

/// The plate of a field drawn into `field` — the focus stop's rect.
pub fn plate(field: Rect) -> Rect {
    Rect::new(field.x, field.y + LABEL_H + LABEL_GAP, field.w, PLATE_H)
}

/// The text a field shows for `value`: the value itself, or one dot per character when secret.
pub fn shown(value: &str, secret: bool) -> String {
    if secret {
        std::iter::repeat_n(SECRET_DOT, value.chars().count()).collect()
    } else {
        value.to_owned()
    }
}

/// The caret's byte offset in [`shown`]'s text for a caret at `caret` in `value`.
fn shown_caret(value: &str, caret: usize, secret: bool) -> usize {
    let mut caret = caret.min(value.len());
    while !value.is_char_boundary(caret) {
        caret -= 1;
    }
    if secret {
        value[..caret].chars().count() * SECRET_DOT.len_utf8()
    } else {
        caret
    }
}

/// How far the run moves left so the caret stays inside `room`: zero while the run up to the
/// caret fits, else just enough that the caret sits at the right edge.
pub fn scroll_for(head_w: f32, room: f32) -> f32 {
    (head_w + CARET_W - room).max(0.0)
}

/// One field, as drawn this frame.
pub struct TextField<'a> {
    label: &'a CStr,
    value: &'a str,
    caret: usize,
    placeholder: Option<&'a CStr>,
    secret: bool,
    /// The focus cross-fade, 0 idle to 1 focused.
    hot: f32,
    editing: bool,
    caret_on: bool,
}

impl<'a> TextField<'a> {
    pub fn new(label: &'a CStr, value: &'a str) -> Self {
        Self { label, value, caret: value.len(), placeholder: None, secret: false, hot: 0.0, editing: false, caret_on: false }
    }
    pub fn caret(mut self, caret: usize) -> Self {
        self.caret = caret;
        self
    }
    pub fn placeholder(mut self, placeholder: &'a CStr) -> Self {
        self.placeholder = Some(placeholder);
        self
    }
    pub fn secret(mut self, secret: bool) -> Self {
        self.secret = secret;
        self
    }
    pub fn hot(mut self, hot: f32) -> Self {
        self.hot = hot.clamp(0.0, 1.0);
        self
    }
    /// The keyboard is up for this field; `caret_on` is the caret's blink phase.
    pub fn editing(mut self, editing: bool, caret_on: bool) -> Self {
        self.editing = editing;
        self.caret_on = caret_on;
        self
    }

    /// Draw into `field` (a [`FIELD_H`]-tall rect).
    pub fn draw(&self, p: Painter, field: Rect, measure: &dyn Measure) {
        Label::new(self.label.as_ptr(), theme::size::CAPTION, theme::TEXT_SECONDARY)
            .v(VAlign::Middle)
            .draw(p, Rect::new(field.x, field.y, field.w, LABEL_H));
        let plate = plate(field);
        let face = theme::cross(theme::CONTROL_IDLE_FILL, theme::ACCENT, self.hot);
        p.rrect(plate, crate::table::PILL_RAD, crate::table::PILL_RAD, face);

        let room = plate.w - 2.0 * PLATE_PAD;
        let ink = theme::cross(theme::TEXT_PRIMARY, theme::ACCENT_INK, self.hot);
        let text_box = Rect::new(plate.x + PLATE_PAD, plate.y, room, plate.h);
        if self.value.is_empty() {
            if let Some(placeholder) = self.placeholder {
                let dim = theme::cross(theme::TEXT_TERTIARY, theme::ROW_VALUE_INK_ON_DIM, self.hot);
                let fitted = measure.fit_line(&placeholder.to_string_lossy(), room, VALUE_SIZE, false);
                Label::new(fitted.as_ptr(), VALUE_SIZE, dim).draw(p, text_box);
            }
            if self.editing && self.caret_on {
                self.draw_caret(p, text_box.x, plate, ink);
            }
            return;
        }
        let shown = CString::new(shown(self.value, self.secret)).unwrap_or_default();
        let at = shown_caret(self.value, self.caret, self.secret);
        let head = CString::new(&shown.to_bytes()[..at]).unwrap_or_default();
        let head_w = measure.width(&head, VALUE_SIZE, false);
        let shift = if self.editing { scroll_for(head_w, room) } else { 0.0 };
        let _clip = ClipScope::open_in(p, text_box);
        let p = p.clipped(text_box);
        Label::new(shown.as_ptr(), VALUE_SIZE, ink).draw(p, Rect::new(text_box.x - shift, text_box.y, room + shift, text_box.h));
        if self.editing && self.caret_on {
            self.draw_caret(p, text_box.x - shift + head_w, plate, ink);
        }
    }

    fn draw_caret(&self, p: Painter, x: f32, plate: Rect, ink: [f32; 4]) {
        let (cap_top, baseline) = plx_gfx::text::text_cap_band(VALUE_SIZE, 0);
        let y = plx_gfx::text::text_vcenter_y(VALUE_SIZE, 0, plate.cy());
        p.rect(Rect::new(x, y + cap_top, CARET_W, baseline - cap_top), 0.0, ink, ink, 0.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_secret_shows_one_dot_per_character_and_never_the_text() {
        assert_eq!(shown("hunter2", true), "•••••••");
        assert_eq!(shown("pässwörd", true).chars().count(), 8, "per character, not per byte");
        assert_eq!(shown("alice", false), "alice");
        assert_eq!(shown("", true), "");
    }

    #[test]
    fn the_caret_lands_on_the_same_character_in_what_is_shown() {
        assert_eq!(shown_caret("pässwörd", 3, true), 2 * SECRET_DOT.len_utf8(), "after `pä`");
        assert_eq!(shown_caret("pässwörd", 2, true), SECRET_DOT.len_utf8(), "inside `ä` falls back to before it");
        assert_eq!(shown_caret("alice", 9, false), 5, "clamped to the end");
    }

    #[test]
    fn the_run_scrolls_only_once_the_caret_would_leave_the_plate() {
        assert_eq!(scroll_for(100.0, 400.0), 0.0);
        assert_eq!(scroll_for(500.0, 400.0), 100.0 + CARET_W);
    }

    #[test]
    fn the_plate_sits_under_the_label_band_and_fills_the_field() {
        let field = Rect::new(10.0, 20.0, 600.0, FIELD_H);
        let plate = plate(field);
        assert_eq!((plate.x, plate.y, plate.w, plate.h), (10.0, 20.0 + LABEL_H + LABEL_GAP, 600.0, PLATE_H));
        assert_eq!(plate.y + plate.h, field.y + field.h);
    }
}
