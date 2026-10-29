//! **One line of editable text with a caret** — the Search query field, and Settings > Services >
//! Seerr's server address. The model is [`crate::text_buffer::TextBuffer`] (the television's own
//! keyboard edits it); this is the picture: a run of text that slides left so the caret never leaves
//! the box, a placeholder that stands in for an empty run (with the caret at the box's start, not
//! after the placeholder), and a caret that blinks while the field is being edited.
//!
//! [`FieldRun`] is the measured half, re-measured only when the text, the caret or the placeholder
//! changes ([`FieldRun::prepare`], a page's `prepare`); [`draw`] is the painted half. The ink, the
//! box and what goes after the caret are the caller's.
use crate::label::Label;
use crate::screen::DrawFrame;
use crate::{Painter, Rect};
use plx_machine::machine::{Host, Measure};
use std::ffi::{CStr, CString};

/// The caret bar's width.
pub const CARET_W: f32 = 5.0;
/// The space between the run and the caret.
pub const CARET_GAP: f32 = 8.0;

/// A field's text as drawn: the run (the text, or the placeholder while it is blank), the part of
/// it before the caret, and their measured widths.
#[derive(Default)]
pub struct FieldRun {
    text: String,
    caret: usize,
    placeholder: CString,
    size: i32,
    bold: bool,
    run: CString,
    run_w: f32,
    head_w: f32,
}

impl FieldRun {
    /// Measure `text` with its caret (a byte offset, brought back to a character boundary and
    /// into the text before any NUL) at `size`, `placeholder` standing in while the text is blank.
    /// Measures only when one of them changed; answers whether it did.
    pub fn prepare(&mut self, text: &str, caret: usize, placeholder: &CStr, size: i32, bold: bool, measure: &dyn Measure) -> bool {
        let (run, head) = run_and_head(text, caret);
        if self.text == run && self.caret == head.len() && self.placeholder.as_c_str() == placeholder
            && self.size == size && self.bold == bold && !self.run.is_empty() {
            return false;
        }
        self.text = run.into();
        self.caret = head.len();
        self.placeholder = placeholder.into();
        self.size = size;
        self.bold = bold;
        self.run = if self.blank() { self.placeholder.clone() } else { CString::new(run).unwrap_or_default() };
        self.run_w = measure.width(&self.run, size, bold);
        self.head_w = if self.blank() { 0.0 } else { measure.width(&CString::new(head).unwrap_or_default(), size, bold) };
        true
    }

    /// The text (cut at a NUL), as prepared.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Nothing but whitespace: the placeholder is drawn, and the caret sits at the box's start.
    pub fn blank(&self) -> bool {
        self.text.trim().is_empty()
    }
}

/// Paint `field` in `rect` with `ink`, its caret shown while `editing` and `caret_on` (the blink's
/// on phase). The run is clipped to the box, with room below it for a descender. Answers the
/// caret's x offset from `rect.x`, so a caller can draw after it (Search's "one more character").
pub fn draw<H: Host>(f: &mut DrawFrame<'_, '_, H>, p: Painter, rect: Rect, field: &FieldRun, ink: [f32; 4],
    caret_ink: [f32; 4], editing: bool, caret_on: bool) -> f32
{
    let (run_dx, caret_dx) = run_layout(field.run_w, run_caret_w(field.blank(), field.head_w), rect.w, editing);
    let (cap_top, cap_base) = plx_gfx::text::text_cap_band(field.size, 1);
    let _clip = f.clip(p, clip_rect(rect, field.size));
    let label = Label::new(field.run.as_ptr(), field.size, ink);
    let label = if field.bold { label.bold() } else { label };
    label.draw(p, Rect::new(rect.x + run_dx, rect.y, rect.w, rect.h));
    if caret_shown(editing, caret_on) {
        let text_y = plx_gfx::text::text_vcenter_y(field.size, 1, rect.cy());
        p.rect(Rect::new(rect.x + caret_dx, text_y + cap_top, CARET_W, cap_base - cap_top), 0.0, caret_ink, caret_ink, 0.0);
    }
    caret_dx
}

/// The clip a field of text `size` in `rect` draws under: the box, and below it the room a
/// descender needs ([`descent_pad`]). A caller drawing beside the caret clips the same.
pub fn clip_rect(rect: Rect, size: i32) -> Rect {
    let (cap_top, cap_base) = plx_gfx::text::text_cap_band(size, 1);
    let pad = descent_pad(rect.h, plx_gfx::text::text_height(size, 1), cap_top, cap_base);
    Rect::new(rect.x, rect.y, rect.w, rect.h + pad)
}

/// Where the run starts (an offset from the box's left edge, `<= 0`) and where the caret is drawn,
/// for a run `run` wide whose caret is `caret` into it, in a box `width` wide. The run slides left
/// only as far as it must to keep the CARET in the box (not the run's tail), and while editing it
/// leaves room for the caret bar and its gap.
pub fn run_layout(run: f32, caret: f32, width: f32, editing: bool) -> (f32, f32) {
    let available = (width - if editing { CARET_W + CARET_GAP } else { 0.0 }).max(0.0);
    let overflow = (caret - available).max(0.0).min((run - available).max(0.0));
    (-overflow, (caret - overflow + CARET_GAP).min((width - CARET_W).max(0.0)))
}

/// The run (the text before any NUL) and the part of it before the caret, which is never split
/// inside a character.
pub fn run_and_head(text: &str, caret: usize) -> (&str, &str) {
    let run = &text[..text.find('\0').unwrap_or(text.len())];
    let mut caret = caret.min(run.len());
    while caret > 0 && !run.is_char_boundary(caret) {
        caret -= 1;
    }
    (run, &run[..caret])
}

/// The caret's distance into the run: none while the placeholder stands in.
pub fn run_caret_w(blank: bool, head_w: f32) -> f32 {
    if blank {
        0.0
    } else {
        head_w
    }
}

pub fn caret_shown(editing: bool, phase_on: bool) -> bool {
    editing && phase_on
}

/// How far below the box the clip reaches: the part of the text's full height that vertical
/// centring on the cap band does not already keep inside it (a descender).
pub fn descent_pad(box_h: f32, full_h: f32, cap_top: f32, cap_base: f32) -> f32 {
    (full_h - box_h * 0.5 - (cap_top + cap_base) * 0.5).max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_caret_is_visible_only_during_the_on_phase_of_an_editing_field() {
        assert!(caret_shown(true, true));
        assert!(!caret_shown(true, false));
        assert!(!caret_shown(false, true));
        assert!(!caret_shown(false, false));
    }

    #[test]
    fn a_run_that_fits_starts_at_the_edge_and_the_caret_trails_it() {
        let (run, caret) = run_layout(100.0, 100.0, 756.0, true);
        assert_eq!(run, 0.0);
        assert_eq!(caret, 100.0 + CARET_GAP);
    }

    #[test]
    fn an_overlong_run_slides_left_so_the_caret_lands_on_the_boxs_right_edge() {
        let box_w = 756.0;
        let (run, caret) = run_layout(900.0, 900.0, box_w, true);
        assert!(run < 0.0);
        assert_eq!(run + 900.0, box_w - CARET_GAP - CARET_W);
        assert_eq!(caret + CARET_W, box_w);
    }

    #[test]
    fn with_no_caret_the_run_uses_the_whole_box() {
        let box_w = 756.0;
        let (run, _) = run_layout(900.0, 900.0, box_w, false);
        assert_eq!(run + 900.0, box_w);
    }

    #[test]
    fn an_empty_run_keeps_the_designs_caret_gap() {
        let (run, caret) = run_layout(0.0, 0.0, 756.0, true);
        assert_eq!(run, 0.0);
        assert_eq!(caret, CARET_GAP);
    }

    #[test]
    fn the_caret_follows_the_insertion_point_whether_or_not_the_run_has_slid() {
        for run_w in [0.0, 1.0, 400.0, 753.0, 900.0, 5000.0] {
            let (run, caret) = run_layout(run_w, run_w, 756.0, true);
            assert_eq!(caret, run + run_w + CARET_GAP, "run_w={run_w}");
        }
    }

    #[test]
    fn the_run_scrolls_to_the_caret_rather_than_to_the_tail() {
        let box_w = 756.0;
        let avail = box_w - CARET_GAP - CARET_W;
        assert_eq!(run_layout(2000.0, 0.0, box_w, true), (0.0, CARET_GAP));
        assert_eq!(run_layout(2000.0, avail - 10.0, box_w, true), (0.0, avail - 10.0 + CARET_GAP));
        let (run, caret) = run_layout(2000.0, 1000.0, box_w, true);
        assert_eq!(run, -(1000.0 - avail));
        assert_eq!(caret + CARET_W, box_w);
        for c in [0.0, 1.0, 500.0, 1999.0, 2000.0] {
            let (_, caret) = run_layout(2000.0, c, box_w, true);
            assert!((0.0..=box_w - CARET_W).contains(&caret), "caret at {c} drew at {caret}");
        }
    }

    #[test]
    fn the_head_is_the_text_before_the_insertion_point() {
        assert_eq!(run_and_head("wallace", 0), ("wallace", ""));
        assert_eq!(run_and_head("wallace", 3), ("wallace", "wal"));
        assert_eq!(run_and_head("wallace", 7), ("wallace", "wallace"));
        assert_eq!(run_and_head("wallace ", 8), ("wallace ", "wallace "));
        assert_eq!(run_and_head("", 0), ("", ""));
    }

    #[test]
    fn a_caret_inside_a_multi_byte_character_never_splits_it() {
        for q in ["суббота", "千と千尋", "Amélie", "🎬🎬", "aé千🎬z"] {
            for c in 0..=q.len() + 4 {
                let (run, head) = run_and_head(q, c);
                assert_eq!(run, q);
                assert!(q.starts_with(head), "the head must be a prefix: {head:?}");
                assert!(head.len() <= c.min(q.len()));
                assert!(c.min(q.len()) - head.len() < 4);
            }
        }
    }

    #[test]
    fn a_nul_cuts_the_run_and_the_caret_lands_inside_what_is_left() {
        assert_eq!(run_and_head("wal\0lace", 3), ("wal", "wal"));
        assert_eq!(run_and_head("wal\0lace", 8), ("wal", "wal"));
        assert_eq!(run_and_head("\0wallace", 4), ("", ""));
        assert_eq!(run_and_head("су\0ббота", 99), ("су", "су"));
    }

    #[test]
    fn a_box_too_narrow_for_the_caret_does_not_go_negative() {
        assert_eq!(run_layout(0.0, 0.0, 2.0, true), (0.0, 0.0));
    }

    #[test]
    fn a_blank_field_puts_the_caret_at_the_field_start_not_after_the_placeholder() {
        assert_eq!(run_caret_w(true, 999.0), 0.0);
        assert_eq!(run_caret_w(false, 42.0), 42.0);
        let (run, caret) = run_layout(480.0, run_caret_w(true, 0.0), 756.0, true);
        assert_eq!(run, 0.0);
        assert_eq!(caret, CARET_GAP);
        let (buggy_run, buggy_caret) = run_layout(480.0, 480.0, 756.0, true);
        assert_eq!(buggy_run, 0.0);
        assert_eq!(buggy_caret, 480.0 + CARET_GAP);
        assert!(caret < buggy_caret);
    }

    #[test]
    fn the_clip_reserves_only_the_descent_centering_does_not_already_clear() {
        assert_eq!(descent_pad(80.0, 87.0, 17.0, 70.0), 3.5);
        assert_eq!(descent_pad(80.0, 40.0, 17.0, 20.0), 0.0);
        assert_eq!(descent_pad(80.0, 10.0, 17.0, 70.0), 0.0);
    }

    /// Measures every byte as 10 px, so a width says which string was measured.
    struct Tens;
    impl Measure for Tens {
        fn width(&self, s: &CStr, _size: i32, _bold: bool) -> f32 {
            s.to_bytes().len() as f32 * 10.0
        }
        fn cap_h(&self, size: i32) -> f32 {
            size as f32
        }
        fn line_h(&self, size: i32) -> f32 {
            size as f32
        }
    }

    #[test]
    fn a_run_is_measured_once_per_change_and_a_blank_one_shows_the_placeholder() {
        let mut run = FieldRun::default();
        assert!(run.prepare("", 0, c"Address", 30, false, &Tens));
        assert!(run.blank());
        assert_eq!((run.run_w, run.head_w), (70.0, 0.0), "the placeholder is drawn, the caret sits at the start");
        assert!(!run.prepare("", 0, c"Address", 30, false, &Tens), "nothing changed, nothing measured");
        assert!(run.prepare("seerr.lan", 5, c"Address", 30, false, &Tens));
        assert_eq!((run.text(), run.run_w, run.head_w), ("seerr.lan", 90.0, 50.0));
        assert!(run.prepare("seerr.lan", 5, c"Address", 30, true, &Tens), "bold measures again");
    }
}
