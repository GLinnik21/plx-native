//! Reusable two-choice decision alert.  It owns focus, hit geometry and destructive styling; a
//! caller supplies the question, the two verbs and — for a question whose consequences its verb
//! does not state — an optional body, which this module measures and which grows the panel.
//!
//! [`layout`] is the pure half, `layout(question_h, body_h)`, and `body_h == 0.0` is byte-identical
//! to the geometry that existed before bodies did (`no_body_leaves_the_geometry_untouched`), which
//! is the shape an alert takes when it asks its question and nothing more. No shipping alert does
//! that today — the last bare one was `ui::exit_alert`, retired 2026-09-03 when BACK at a root
//! stopped asking anything and started handing the screen back to the television, and the one that
//! ships (Privacy & data's *Delete local data*) carries a body — so that test is now the ONLY thing
//! holding the body-free arithmetic, and it is kept deliberately: the next question this app asks
//! will almost certainly be a bare one. The body is held on the ALERT
//! rather than passed to [`DecisionAlert::draw`] because it moves the controls, and
//! [`DecisionAlert::press_at`] has to compute the same panel the last draw did.
//!
//! **Every interactive exit — confirm, cancel, or BACK — takes [`DecisionAlert::dismiss`], never
//! [`DecisionAlert::close`].** `close` is [`Popover::close`]'s case: a subject that vanished out
//! from under the alert, or a defensive reset before opening it fresh (`consent`'s `open`/
//! `open_settings` both call it for exactly that, unconditionally, before the alert has ever shown
//! anyone anything). A person's OWN answer is not that case, however destructive: the alert has
//! already been seen and already been answered, and the caller learns the choice from
//! [`DecisionAlert::choice`] before anything the answer authorizes runs — so there is always a
//! frame in which fading it out costs nothing but a few frames of a panel already leaving. Reported
//! off `consent.rs`'s "Delete all local data" flow (2026-09-03): confirming jumped straight to an
//! instant hide because the confirm arm reused the screen's own defensive `close()` reset instead
//! of `dismiss()` — this alert had the same appearance animation `exit_alert` shares, and none
//! leaving. A
//! caller that tears its OWN host down synchronously under the alert (as that same confirm arm
//! still does, to the Settings screen behind it) may keep doing so; the alert's fade then simply
//! runs over whatever the app shows next, exactly like a dismissed `account_menu`/`item_menu`
//! fading over the host it returned to.

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
/// its question and nothing more, and the arithmetic is written so that case stays byte-identical
/// to the layout that existed before a body was possible.
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
            // The page under a decision alert is frozen into the shared host snapshot
            // (`popover::host`) — the alert's own contents are a question and two answers, and
            // what made "Cancel / Delete" lag was the full page being re-rendered under it on every
            // frame its focus spring moved. Over the Settings family (the delete-data alert) the
            // page closure is already skipped and nothing is captured, which costs nothing; over
            // Home (the exit alert) it is the difference between the page's ~50 ms and one quad.
            pop: Popover::new().caching_host(),
            choice: Choice::Cancel,
            controls: CtlPop::new(),
            body: None,
        }
    }
    pub(crate) fn is_open(&self) -> bool {
        self.pop.is_open()
    }
    /// **Has the sheet finished arriving?** A key acts on the logical state, but a POINTER hit is
    /// positional and [`Self::press_at`] tests FINAL coordinates while the sheet is still drawn
    /// through the entrance painter — so an immediate click lands on a control that is displaced
    /// and nearly invisible. Every pointer caller gates on this (`ui::route_screen`'s rule 11).
    pub(crate) fn settled(&self) -> bool {
        self.pop.appear_settled()
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
        // The ring moved and nothing behind the alert did — `popover::note_own_damage`.
        crate::ui::popover::note_own_damage();
        crate::ui::idle::invalidate();
    }
    pub(crate) fn move_focus(&mut self, delta: i32) {
        self.choice = if delta > 0 {
            Choice::Destructive
        } else {
            Choice::Cancel
        };
        crate::ui::popover::note_own_damage();
        crate::ui::idle::invalidate();
    }
    pub(crate) fn update(&mut self, dt: f32) {
        if !self.visible() {
            return;
        }
        // The appear spring and the two answers' focus pops are the ALERT's motion, not the
        // page's — `popover::own_motion`.
        let _own = crate::ui::popover::own_motion();
        self.pop.update(dt);
        self.controls.step(
            Some(matches!(self.choice, Choice::Destructive) as usize),
            dt,
        );
    }
    pub(crate) fn draw_scrim(&self) {
        if self.visible() {
            // Live over the frozen host — and the first `live` of a frame is what takes the
            // snapshot, before this scrim lands on it. See `popover::host::live`.
            let _live = crate::ui::popover::host::live();
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
        // Everything below is LIVE over the frozen host page — see `popover::host::live`.
        let _live = crate::ui::popover::host::live();
        let l = self.measured();
        let p = self.pop.content_painter(Popover::RISE);
        crate::ui::profile::phase("da.panel", || self.pop.panel(p, l.panel, theme::ALERT_PANEL_RAD));
        crate::ui::profile::phase("da.text", || {
            Label::new(question.as_ptr(), theme::size::TITLE, theme::TEXT_PRIMARY)
                .bold()
                .h(HAlign::Center)
                .draw(p, l.question);
            if let Some(body) = self.body {
                Self::body_view(body).draw(p, l.body);
            }
        });
        let env = Env::inert();
        crate::ui::profile::phase("da.ctl", || {
        Button::new(cancel.as_ptr(), theme::size::BODY, l.cancel)
            .focused(self.choice == Choice::Cancel)
            .scale(self.controls.scale(0))
            .draw(&env, p);
        Button::new(destructive.as_ptr(), theme::size::BODY, l.destructive)
            .style(ControlStyle::Danger)
            .focused(self.choice == Choice::Destructive)
            .scale(self.controls.scale(1))
            .draw(&env, p);
        });
    }
    pub(crate) fn press_at(&mut self, x: f32, y: f32) -> bool {
        let l = self.measured();
        if l.cancel.contains(x, y) {
            self.set_choice(Choice::Cancel);
            true
        } else if l.destructive.contains(x, y) {
            self.set_choice(Choice::Destructive);
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    /// **A body must not move an alert that has none.** The body arithmetic has to vanish
    /// completely at `body_h == 0.0` — not merely add a small gap. Written by computing the panel
    /// both ways and comparing.
    ///
    /// Every shipping alert carries a body today — `ui::exit_alert`, which asked its question
    /// alone, was retired on 2026-09-03 along with the root-BACK question itself — so this test is
    /// the whole of what holds the body-free case. It stays for that reason rather than in spite of
    /// it: arithmetic nothing exercises is arithmetic that drifts.
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
