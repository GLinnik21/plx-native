//! Pure text-measurement paths that bypass SDL2_ttf: diagnostic field wrapping and `StatusOverlay`/`Button` measured layout.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;

/// Diagnostics are support evidence: the right edge may never turn an unfamiliar opaque token
/// into a plausible-looking prefix. Unit tests have no SDL_ttf, so this specifically grades
/// the conservative fallback used by the schema/height tests.
#[test]
fn diagnostic_wrapping_preserves_even_one_oversized_word() {
    let value = "abcdefghijklmnopqrstuvwxyz";
    let width = FIELD_KEY_W + theme::space::SM + 4.0 * VAL_AVG_ADVANCE;
    let lines = value_lines(value, width);
    assert!(lines.len() > 1, "the token did not wrap: {lines:?}");
    assert!(
        lines.iter().all(|line| line.chars().count() <= 4),
        "a line escaped its frame: {lines:?}"
    );
    assert_eq!(
        lines.concat(),
        value,
        "wrapping dropped or changed evidence"
    );
}

#[test]
fn diagnostic_wrapping_preserves_bounded_sentences() {
    let value = "conservative horizon 116 s · risk 4%";
    let width = FIELD_KEY_W + theme::space::SM + 12.0 * VAL_AVG_ADVANCE;
    let lines = value_lines(value, width);
    assert_eq!(
        lines.join(" "),
        value,
        "wrapping dropped or changed evidence"
    );
    assert!(
        lines.iter().all(|line| line.chars().count() <= 12),
        "a line escaped its frame: {lines:?}"
    );
}

#[test]
fn measured_status_action_uses_shared_button_width_height_and_reason_spacing() {
    let frame = Rect::new(100.0, 200.0, 600.0, 500.0);
    let plain = StatusOverlay::new(frame, c"Unavailable", StatusKind::Failed).action(c"Try again");
    let action = plain.action_frame_measured(&StatusMetrics).unwrap();
    assert_eq!(action.w, 96.0 + BTN_PILL_AIR);
    assert_eq!(action.h, StatusOverlay::CTRL_H);
    assert_eq!(action.cx(), frame.cx());
    // A bounded Failed read-out centres its TITLE-rung verdict band (52) on the frame.
    assert_eq!(action.y, frame.cy() + 26.0 + theme::space::LG);
    let explained = plain.reason(c"Shared source");
    let shifted = explained.action_frame_measured(&StatusMetrics).unwrap();
    // …and its reason is the design system's two-line BODY slot: one pitch plus one line box.
    let slot = theme::size::BODY as f32 * 1.32 + 40.0;
    assert!((shifted.y - action.y - (theme::space::SM + slot)).abs() < 1e-3);
    let bands = explained.bands_measured(&StatusMetrics);
    let drawn = explained.action_rect(
        Button::pill_w_measured(c"Try again", STATUS_CAP_SZ, false, false, &StatusMetrics), &bands);
    assert_eq!((drawn.x, drawn.y, drawn.w, drawn.h), (shifted.x, shifted.y, shifted.w, shifted.h));
    // A Working read-out keeps its one-line CAPTION reason under a BODY verdict.
    let working = StatusOverlay::new(frame, c"Loading", StatusKind::Working).action(c"Try again");
    let w0 = working.action_frame_measured(&StatusMetrics).unwrap();
    let w1 = working.reason(c"Slow").action_frame_measured(&StatusMetrics).unwrap();
    assert_eq!(w1.y - w0.y, theme::space::SM + 28.0);
}

/// **A failed read-out never scolds** — the design system's `StatusOverlay` contract ("the app
/// does not scold"): its verdict is bold `size::TITLE` in `TEXT_SECONDARY` and its reason regular
/// `size::BODY` in `TEXT_SECONDARY`. No kind's verdict or reason is ever the danger/red token.
#[test]
fn a_failed_status_verdict_is_never_the_danger_token() {
    assert_eq!(
        StatusOverlay::verdict_face(StatusKind::Failed),
        (theme::size::TITLE, true, theme::TEXT_SECONDARY)
    );
    assert_eq!(StatusOverlay::reason_face(StatusKind::Failed), (theme::size::BODY, theme::TEXT_SECONDARY));
    for kind in [StatusKind::Working, StatusKind::Failed, StatusKind::Empty] {
        let (_, _, ink) = StatusOverlay::verdict_face(kind);
        let (_, reason) = StatusOverlay::reason_face(kind);
        for c in [ink, reason] {
            assert_ne!(c, theme::DANGER, "{kind:?}");
            assert_ne!(c, theme::TEXT_PRIMARY, "{kind:?}: primary is the player's glyph read-out's alone");
        }
    }
    assert_eq!(StatusOverlay::verdict_face(StatusKind::Empty).2, theme::TEXT_TERTIARY);
    assert_eq!(StatusOverlay::verdict_face(StatusKind::Working).2, theme::TEXT_SECONDARY);
}

/// A PAGE-placed Failed read-out hangs its verdict from `FULL_ANCHOR_TOP` (372, the player's
/// glyph line) in screen space whatever frame the caller held, and stacks the reason and the row
/// under it — the row `space::LG` under the reason's two-line slot, or under the verdict when there
/// is no reason — centred on the panel. A bounded one and a Working one still centre in their
/// frame, and `.page()` leaves a Working or Empty read-out alone.
#[test]
fn a_full_frame_failed_readout_hangs_from_the_top_anchor() {
    let content = Rect::new(96.0, 232.0, 1728.0, 848.0);
    let mut rows = Vec::new();
    for frame in [Rect::FULL, content] {
        for reason in [None, Some(c"Why")] {
            let mut full = StatusOverlay::new(frame, c"Couldn't sign in", StatusKind::Failed).action(c"Try again");
            if let Some(r) = reason {
                full = full.reason(r);
            }
            let full = full.page();
            let bands = full.bands_measured(&StatusMetrics);
            assert_eq!(bands.cap.y, StatusOverlay::FULL_ANCHOR_TOP);
            let copy_bottom = bands.reason.map_or(bands.cap.y + bands.cap.h, |r| r.y + r.h);
            let row = full.action_frame_measured(&StatusMetrics).unwrap();
            assert_eq!(row.y, copy_bottom + theme::space::LG, "the row stacks under the copy");
            assert_eq!(row.cx(), Rect::FULL.cx(), "centred on the panel, not the frame");
            rows.push((reason.is_some(), row.y));
        }
    }
    // The same copy shape stands on the same row line whichever frame the caller held.
    assert_eq!(rows[0], rows[2]);
    assert_eq!(rows[1], rows[3]);
    // A full frame alone no longer decides it: the caller says `.page()`.
    let bounded = Rect::new(100.0, 200.0, 600.0, 500.0);
    let b = StatusOverlay::new(bounded, c"Failed", StatusKind::Failed).bands_measured(&StatusMetrics);
    assert_eq!(b.cap.y, bounded.cy() - 26.0);
    let w = StatusOverlay::new(Rect::FULL, c"Loading", StatusKind::Working).page().bands_measured(&StatusMetrics);
    assert_eq!(w.cap.y, Rect::FULL.cy() + theme::space::XS);
    let e = StatusOverlay::new(bounded, c"Nothing", StatusKind::Empty).page();
    assert_eq!((e.page, e.frame.y), (false, bounded.y), "an Empty answer keeps its frame");
}

/// A read-out that offers two things to press lays them out as ONE centred run, `CONTROL_GAP`
/// apart, on the lone action's own row — and the primary is still slot 0.
#[test]
fn measured_status_row_centres_every_pill_on_the_primarys_row() {
    let frame = Rect::new(100.0, 200.0, 600.0, 500.0);
    let lone = StatusOverlay::new(frame, c"Failed", StatusKind::Failed).action(c"Try again");
    let one = lone.action_frame_measured(&StatusMetrics).unwrap();
    let row = StatusOverlay::new(frame, c"Failed", StatusKind::Failed)
        .action(c"Try again")
        .secondary(Some(c"Details"));
    let [a, b] = row.action_frames_measured(&StatusMetrics);
    let (a, b) = (a.unwrap(), b.unwrap());
    let w = 96.0 + BTN_PILL_AIR;
    assert_eq!((a.w, b.w), (w, w));
    assert_eq!((a.y, b.y), (one.y, one.y), "the row is the lone action's row");
    assert_eq!(b.x - (a.x + a.w), CONTROL_GAP);
    assert!(((a.x + b.x + b.w) * 0.5 - frame.cx()).abs() < 0.001, "the row is centred");
    // A secondary does not exist without a primary.
    let orphan = StatusOverlay::new(frame, c"Failed", StatusKind::Failed).secondary(Some(c"Details"));
    assert!(orphan.action_frames_measured(&StatusMetrics).iter().all(Option::is_none));
}

/// The one quiet line sits `space::MD` under the row, in the row's place when there is none, and
/// cannot move the row.
#[test]
fn measured_status_note_sits_under_the_row() {
    let frame = Rect::new(100.0, 200.0, 600.0, 500.0);
    let plain = StatusOverlay::new(frame, c"Failed", StatusKind::Failed).action(c"Try again");
    let noted = StatusOverlay::new(frame, c"Failed", StatusKind::Failed)
        .action(c"Try again")
        .note(Some(c"Report sent"));
    let row = noted.action_frame_measured(&StatusMetrics).unwrap();
    let lone = plain.action_frame_measured(&StatusMetrics).unwrap();
    assert_eq!((row.x, row.y, row.w, row.h), (lone.x, lone.y, lone.w, lone.h));
    let bands = noted.bands_measured(&StatusMetrics);
    let band = noted.note_band(&bands, &StatusMetrics).unwrap();
    assert_eq!(band.y, row.y + row.h + theme::space::MD);
    assert!(plain.note_band(&bands, &StatusMetrics).is_none());
    let bare = StatusOverlay::new(frame, c"Failed", StatusKind::Failed).note(Some(c"Report sent"));
    let bb = bare.bands_measured(&StatusMetrics);
    assert_eq!(bare.note_band(&bb, &StatusMetrics).unwrap().y, bb.action_y);
}

/// A note on its way carries an inline spinner in a leading gutter, and the PAIR is centred: the
/// text starts one gutter after the group's left edge, and the group is as far from the band's
/// left as its end is from the band's right.
#[test]
fn a_busy_note_centres_its_spinner_and_text_as_one_group() {
    let band = Rect::new(100.0, 400.0, 600.0, 28.0);
    let (gutter_x, text) = StatusOverlay::busy_note_split(band, 200.0);
    assert_eq!(text.x - gutter_x, Spinner::inline_gutter());
    assert_eq!((text.y, text.w, text.h), (band.y, 200.0, band.h));
    assert!((gutter_x - band.x - (band.x + band.w - (text.x + text.w))).abs() < 1e-3, "centred as one group");
    let ring = Spinner::leading(gutter_x, band.cy());
    assert_eq!(ring.r, Spinner::R_INLINE);
    assert!((ring.cx - ring.r - ring.dot_r - gutter_x).abs() < 1e-3, "the ring's dots start at the gutter's edge");
    assert!(ring.cx + ring.r + ring.dot_r + theme::space::XS <= text.x + 1e-3, "the ring never touches the text");
    let o = StatusOverlay::new(band, c"Failed", StatusKind::Failed).note(Some(c"Sending")).note_busy(true);
    assert!(o.note_busy);
    // A busy note is one line tall, whatever it says.
    let b = o.bands_measured(&StatusMetrics);
    assert_eq!(o.note_band(&b, &StatusMetrics).unwrap().h, o.note_view(c"").line_h());
}

#[test]
fn measured_status_without_action_does_not_consult_metrics() {
    struct Unused;
    impl crate::ui::machine::Measure for Unused {
        fn width(&self, _: &core::ffi::CStr, _: i32, _: bool) -> f32 { panic!("no action") }
        fn cap_h(&self, _: i32) -> f32 { panic!("no action") }
        fn line_h(&self, _: i32) -> f32 { panic!("no action") }
    }
    assert!(StatusOverlay::new(Rect::FULL, c"Empty", StatusKind::Empty)
        .action_frame_measured(&Unused).is_none());
}

#[test]
fn measured_button_preserves_both_accessory_slots() {
    let plain = Button::pill_w_measured(c"Try again", STATUS_CAP_SZ, false, false, &StatusMetrics);
    let slot = STATUS_CAP_SZ as f32 * BTN_ICON_RATIO + BTN_ICON_GAP;
    for (leading, trailing) in [(true, false), (false, true), (true, true)] {
        let width = Button::pill_w_measured(c"Try again", STATUS_CAP_SZ, leading, trailing, &StatusMetrics);
        assert!((width - plain - slot * (u8::from(leading) + u8::from(trailing)) as f32).abs() < 0.001);
    }
}
