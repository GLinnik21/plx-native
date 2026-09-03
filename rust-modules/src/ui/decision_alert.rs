//! Reusable two-choice decision alert.  It owns focus, hit geometry and destructive styling; a
//! caller supplies the question, the two verbs and — for a question whose consequences its verb
//! does not state — an optional body, which this module measures and which grows the panel.
//!
//! [`layout`] is the pure half, `layout(question_h, body_h)`, and `body_h == 0.0` is byte-identical
//! to the geometry that existed before bodies did (`no_body_leaves_the_geometry_untouched`) —
//! `exit_alert` shares this layout and asks its question alone. The body is held on the ALERT
//! rather than passed to [`DecisionAlert::draw`] because it moves the controls, and
//! [`DecisionAlert::press_at`] has to compute the same panel the last draw did.

use crate::ui::label::{HAlign, Label};
use crate::ui::popover::Popover;
use crate::ui::text_view::TextView;
use crate::ui::widgets::{Button, ControlStyle, CtlPop, StatusOverlay};
use crate::ui::{theme, Env, Rect, View};

const PANEL_W: f32 = 660.0;
const PAD_TOP: f32 = theme::alert::PAD;
const PAD_X: f32 = theme::alert::PAD;
const PAD_BOTTOM: f32 = 34.0;
const QUESTION_GAP: f32 = theme::space::LG;
/// Question → body. Smaller than [`QUESTION_GAP`], which separates the whole text block from the
/// controls: the body belongs to the question, not to the buttons.
const BODY_GAP: f32 = theme::space::SM;
/// The body's measuring width — the panel minus its side padding, so a caller can measure without
/// reaching into the layout.
pub(crate) const BODY_W: f32 = PANEL_W - 2.0 * PAD_X;
const BUTTON_W: f32 = 260.0;
const BUTTON_GAP: f32 = 20.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Choice {
    Cancel,
    Destructive,
}

#[derive(Clone, Copy)]
pub(crate) struct Layout {
    pub(crate) panel: Rect,
    pub(crate) question: Rect,
    /// Zero-height when the alert has no body, in which case every other rect is exactly what it
    /// was before bodies existed — see `no_body_leaves_the_geometry_untouched`.
    pub(crate) body: Rect,
    pub(crate) cancel: Rect,
    pub(crate) destructive: Rect,
}

/// The panel, **pure**, from the two measured text heights. `body_h` is 0.0 for an alert that asks
/// its question and nothing more (`exit_alert`), and the arithmetic is written so that case stays
/// byte-identical to the layout that existed before a body was possible.
pub(crate) fn layout(question_h: f32, body_h: f32) -> Layout {
    let body_block = if body_h > 0.0 { BODY_GAP + body_h } else { 0.0 };
    let h = PAD_TOP + question_h + body_block + QUESTION_GAP + StatusOverlay::CTRL_H + PAD_BOTTOM;
    let panel = Rect::new(
        (Rect::FULL.w - PANEL_W) * 0.5,
        (Rect::FULL.h - h) * 0.5,
        PANEL_W,
        h,
    );
    let row_w = BUTTON_W * 2.0 + BUTTON_GAP;
    let x = panel.cx() - row_w * 0.5;
    let by = panel.y + PAD_TOP + question_h + body_block + QUESTION_GAP;
    Layout {
        panel,
        question: Rect::new(
            panel.x + PAD_X,
            panel.y + PAD_TOP,
            panel.w - 2.0 * PAD_X,
            question_h,
        ),
        body: Rect::new(
            panel.x + PAD_X,
            panel.y + PAD_TOP + question_h + BODY_GAP,
            BODY_W,
            body_h,
        ),
        cancel: Rect::new(x, by, BUTTON_W, StatusOverlay::CTRL_H),
        destructive: Rect::new(
            x + BUTTON_W + BUTTON_GAP,
            by,
            BUTTON_W,
            StatusOverlay::CTRL_H,
        ),
    }
}

pub(crate) struct DecisionAlert {
    pop: Popover,
    choice: Choice,
    controls: CtlPop<2>,
    /// The optional paragraph under the question, held on the ALERT rather than passed to
    /// [`draw`](Self::draw). It has to be here because the body changes where the buttons are, and
    /// [`press_at`](Self::press_at) must compute the same panel as the last draw did. A body passed
    /// per-frame would be correct for drawing and wrong for the first hit test after an open, which
    /// is precisely the frame a fast click lands on.
    body: Option<&'static str>,
}

impl DecisionAlert {
    pub(crate) const fn new() -> Self {
        Self {
            pop: Popover::new(),
            choice: Choice::Cancel,
            controls: CtlPop::new(),
            body: None,
        }
    }
    pub(crate) fn is_open(&self) -> bool {
        self.pop.is_open()
    }
    /// Ask the question alone.
    pub(crate) fn open(&mut self) {
        self.body = None;
        self.open_inner();
    }
    /// Ask it with a paragraph underneath — for a question whose consequences are not fully stated
    /// by its verb. The destructive delete is the case: "Delete all local data" is accurate about
    /// what it removes and silent about what it CANNOT reach, and the confirmation is the last
    /// moment that difference can be stated.
    pub(crate) fn open_with_body(&mut self, body: &'static str) {
        self.body = Some(body);
        self.open_inner();
    }
    fn open_inner(&mut self) {
        self.choice = Choice::Cancel;
        self.pop.open();
        crate::ui::idle::invalidate();
    }
    /// The panel as last drawn. **Not callable from a host test** — `text_height` and `measure_h`
    /// both reach SDL2_ttf; the pure half is [`layout`].
    fn measured(&self) -> Layout {
        let qh = crate::text::text_height(theme::size::TITLE, 1);
        let bh = self
            .body
            .map_or(0.0, |b| Self::body_view(b).measure_h(BODY_W));
        layout(qh, bh)
    }
    fn body_view(text: &str) -> TextView<'_> {
        TextView::new(text, theme::size::BODY, theme::TEXT_READING)
            .h(HAlign::Center)
            .max_lines(4)
    }
    /// Instant hide — see [`Popover::close`]. Interactive answers take [`dismiss`](Self::dismiss).
    pub(crate) fn close(&mut self) {
        self.pop.close();
        crate::ui::idle::invalidate();
    }
    /// The shared exit choreography, in reverse of the entry (`Popover::dismiss`).
    pub(crate) fn dismiss(&mut self) {
        self.pop.dismiss();
        crate::ui::idle::invalidate();
    }
    /// Open or still fading out — the DRAW gate, never the input gate.
    pub(crate) fn visible(&self) -> bool {
        self.pop.visible()
    }
    pub(crate) fn choice(&self) -> Choice {
        self.choice
    }
    pub(crate) fn set_choice(&mut self, choice: Choice) {
        self.choice = choice;
        crate::ui::idle::invalidate();
    }
    pub(crate) fn move_focus(&mut self, delta: i32) {
        self.choice = if delta > 0 {
            Choice::Destructive
        } else {
            Choice::Cancel
        };
        crate::ui::idle::invalidate();
    }
    pub(crate) fn update(&mut self, dt: f32) {
        if !self.visible() {
            return;
        }
        self.pop.update(dt);
        self.controls.step(
            Some(matches!(self.choice, Choice::Destructive) as usize),
            dt,
        );
    }
    pub(crate) fn draw_scrim(&self) {
        if self.visible() {
            self.pop.scrim(0.55);
        }
    }
    pub(crate) fn draw(
        &mut self,
        question: &core::ffi::CStr,
        cancel: &core::ffi::CStr,
        destructive: &core::ffi::CStr,
    ) {
        if !self.visible() {
            return;
        }
        let l = self.measured();
        let p = self.pop.content_painter(Popover::RISE);
        self.pop.panel(p, l.panel, theme::ALERT_PANEL_RAD);
        Label::new(question.as_ptr(), theme::size::TITLE, theme::TEXT_PRIMARY)
            .bold()
            .h(HAlign::Center)
            .draw(p, l.question);
        if let Some(body) = self.body {
            Self::body_view(body).draw(p, l.body);
        }
        let env = Env::inert();
        Button::new(cancel.as_ptr(), theme::size::BODY, l.cancel)
            .focused(self.choice == Choice::Cancel)
            .scale(self.controls.scale(0))
            .draw(&env, p);
        Button::new(destructive.as_ptr(), theme::size::BODY, l.destructive)
            .style(ControlStyle::Danger)
            .focused(self.choice == Choice::Destructive)
            .scale(self.controls.scale(1))
            .draw(&env, p);
    }
    pub(crate) fn press_at(&mut self, x: f32, y: f32) -> bool {
        let l = self.measured();
        if l.cancel.contains(x, y) {
            self.choice = Choice::Cancel;
            true
        } else if l.destructive.contains(x, y) {
            self.choice = Choice::Destructive;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    /// **A body must not move an alert that has none.** `exit_alert` shares this layout and asks
    /// its question alone, so the body arithmetic has to vanish completely at `body_h == 0.0` —
    /// not merely add a small gap. Written by computing the panel both ways and comparing.
    #[test]
    fn no_body_leaves_the_geometry_untouched() {
        let plain = layout(40.0, 0.0);
        assert_eq!(plain.body.h, 0.0);
        // Against the arithmetic as it stood BEFORE a body was possible, written out rather than
        // recomputed from the constants under test — the first version of this compared
        // `plain.panel` to `plain.panel`, which is true of every layout ever written.
        let legacy_h = PAD_TOP + 40.0 + QUESTION_GAP + StatusOverlay::CTRL_H + PAD_BOTTOM;
        assert_eq!(plain.panel.h, legacy_h);
        assert_eq!(plain.panel.y, (Rect::FULL.h - legacy_h) * 0.5);
        assert_eq!(plain.question.y, plain.panel.y + PAD_TOP);
        assert_eq!(plain.question.h, 40.0);
        assert_eq!(plain.cancel.y, plain.panel.y + PAD_TOP + 40.0 + QUESTION_GAP);
        assert_eq!(plain.destructive.y, plain.cancel.y);
    }

    /// A body grows the panel and pushes the controls down by exactly its own height plus its gap,
    /// so the question keeps its position and the buttons keep their distance from the text block.
    #[test]
    fn a_body_grows_the_panel_and_moves_only_what_is_below_it() {
        let plain = layout(40.0, 0.0);
        let with = layout(40.0, 90.0);
        assert_eq!(with.body.h, 90.0);
        assert_eq!(with.panel.h, plain.panel.h + BODY_GAP + 90.0);
        // The question keeps its POSITION within the panel, not merely its height — the earlier
        // version asserted the height, which the body arithmetic could never have changed.
        assert_eq!(with.question.y - with.panel.y, plain.question.y - plain.panel.y);
        assert_eq!(with.question.h, plain.question.h);
        // …and the controls move by exactly the body block, measured against the no-body layout.
        assert_eq!(
            with.cancel.y - with.panel.y,
            (plain.cancel.y - plain.panel.y) + BODY_GAP + 90.0
        );
        assert_eq!(with.cancel.y - with.body.y, 90.0 + QUESTION_GAP);
        assert!(with.body.y > with.question.y);
        assert!(with.cancel.y > with.body.y + with.body.h);
    }

    #[test]
    fn two_actions_share_one_measured_row_and_cancel_is_first() {
        let l = layout(40.0, 0.0);
        assert_eq!(l.cancel.w, l.destructive.w);
        assert_eq!(l.cancel.y, l.destructive.y);
        assert!(l.cancel.x < l.destructive.x);
        assert!(l.question.y + l.question.h < l.cancel.y);
    }
}
