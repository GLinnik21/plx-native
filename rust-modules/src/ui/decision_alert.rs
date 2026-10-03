//! Reusable decision alert — two choices, or a one-answer card. The caller owns focus; this
//! component shares measured geometry for paint and hits, retained paragraphs, and answer styling.
//! Complete translated titles and bodies use the shared left padding edge. Oversized disclosures
//! scroll inside the safe frame while their answers stay visible.
//!
//! [`layout`] is the pure half, `layout(question_h, body_h)`, and `body_h == 0.0` is byte-identical
//! to the geometry that existed before bodies did when the question fits the safe frame
//! (`no_body_leaves_the_geometry_untouched`), which
//! is the shape an alert takes when it asks its question alone. Shipping confirmations and
//! report cards retain their disclosures on the alert, so the body-free test separately guards
//! that simpler geometry. The body is held on the ALERT
//! rather than passed to [`DecisionAlert::draw`] because it moves the controls, and
//! [`DecisionAlert::frames`] — the geometry an owning Engine screen registers its own hit stops
//! from — has to measure the same retained content the draw consumes.
//!
//! **Focus and hit-testing are the owning screen's, not this type's** (restructure phase 12):
//! `DecisionAlert` used to carry its own `move_focus`/`press_at` ladder, driven by a caller that
//! translated a raw key or pointer event into a call here. Engine screens register the alert's
//! answers as ordinary focus-group elements through [`DecisionAlert::frames`] and move this
//! type's selection with [`DecisionAlert::set_choice`], as with any other control. The old
//! two SDL-keysym-shaped methods had no caller left and are gone.
//!
//! **The same sheet is also a one- or two-answer CARD** ([`DecisionAlert::open_card`]): a title,
//! body paragraphs stacked `PARA_GAP` apart, and either the usual two answers or ONE
//! (`Answers::One`), which takes the centred cancel slot while the destructive rect collapses to
//! zero width so no caller can register or draw it. The sign-in failure's *Details* is the caller
//! (its Report ID and support line, *Close*, and *Send report* only while the report can still be
//! sent) — a disclosure that used to grow the read-out under its control row, and was replaced by
//! this card because three buttons and three labels stacked on one page read as a mess (owner,
//! 2026-09-19).
//! A live card feeds its rendered content back through [`DecisionAlert::reconcile_card`] before
//! drawing. Changed paragraphs or answers update in place, invalidate the cached panel ground,
//! and retain any still-valid selection; they never restart the entrance or steal focus for a
//! newly available action. The host's focus engine reconciles a removed action to that selection.
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

use std::borrow::Cow;

use crate::ui::consts::{K_SCROLL, SAFE};
use crate::ui::machine::Measure;
use crate::ui::popover::Popover;
use crate::ui::text_view::TextView;
use crate::ui::widgets::{Button, ControlStyle, CtlPop, StatusOverlay};
use crate::ui::{theme, Env, Rect, Spring, View};

const PANEL_W: f32 = 660.0;
const PAD_TOP: f32 = theme::alert::PAD;
const PAD_X: f32 = theme::alert::PAD;
const PAD_BOTTOM: f32 = 34.0;
const QUESTION_GAP: f32 = theme::space::LG;
/// Question → body. Smaller than [`QUESTION_GAP`], which separates the whole text block from the
/// controls: the body belongs to the question, not to the buttons.
const BODY_GAP: f32 = theme::space::SM;
/// Between two body paragraphs — one rung under [`BODY_GAP`]: they are one disclosure in two
/// parts (a Report ID, then the line support reads it with), not two blocks.
const PARA_GAP: f32 = theme::space::XS;
/// The body's measuring width — the panel minus its side padding, so a caller can measure without
/// reaching into the layout.
pub(crate) const BODY_W: f32 = PANEL_W - 2.0 * PAD_X;
pub(crate) const BUTTON_W: f32 = 260.0;
const BUTTON_GAP: f32 = 20.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Choice {
    Cancel,
    Destructive,
}

/// Which face the second answer wears. **Not every two-choice question ends something** — issue
/// #75's one-off "Send report" needed a second button beside "Not now", and the alert's only look
/// until then was [`ControlStyle::Danger`] for that slot. Shipping "Send report" in the same red
/// [`theme::DANGER`] face `ui::consent`'s *Delete all local data* uses would say the press is
/// destructive when it is the opposite — a report leaves, nothing is lost. `Destructive` is the
/// default and the delete alert's own look is unchanged by this existing at all.
/// How many answers the alert offers. **`One` is an information card, not a question** — the
/// sign-in failure's *Details*, which has nothing to decide once its report has gone and so offers
/// only *Close*. Its one answer is the CANCEL slot, centred on the panel: the slot BACK already
/// means, so the card needs no second vocabulary.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Answers {
    One,
    Two,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Tone {
    Destructive,
    /// Both answers are ordinary controls — no danger tint on either.
    Neutral,
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
    pub(crate) text_viewport: Rect,
    pub(crate) scroll_max: f32,
}

/// The panel, **pure**, from the two measured text heights. `body_h` is 0.0 for an alert that asks
/// its question and nothing more, and the arithmetic is written so that case stays byte-identical
/// to the layout that existed before a body was possible when the question fits the safe frame.
pub(crate) fn layout(question_h: f32, body_h: f32) -> Layout {
    layout_with(question_h, body_h, Answers::Two)
}

/// [`layout`] for either answer count. With [`Answers::One`] the cancel slot is centred on the
/// panel and the destructive rect is the zero-width point at the row's centre — nothing draws
/// there and nothing registers it.
pub(crate) fn layout_with(question_h: f32, body_h: f32, answers: Answers) -> Layout {
    let body_block = if body_h > 0.0 { BODY_GAP + body_h } else { 0.0 };
    let text_h = question_h + body_block;
    let fixed_h = PAD_TOP + QUESTION_GAP + StatusOverlay::CTRL_H + PAD_BOTTOM;
    let overflow = text_h + fixed_h > SAFE.h;
    let viewport_h = text_h.min(SAFE.h - fixed_h);
    let h = fixed_h + viewport_h;
    let panel = Rect::new((Rect::FULL.w - PANEL_W) * 0.5, (Rect::FULL.h - h) * 0.5, PANEL_W, h);
    let row_w = BUTTON_W * 2.0 + BUTTON_GAP;
    let x = panel.cx() - row_w * 0.5;
    let by = panel.y + PAD_TOP + viewport_h + QUESTION_GAP;
    let (cancel, destructive) = if answers == Answers::One {
        (Rect::new(panel.cx() - BUTTON_W * 0.5, by, BUTTON_W, StatusOverlay::CTRL_H),
         Rect::new(panel.cx(), by, 0.0, StatusOverlay::CTRL_H))
    } else {
        (Rect::new(x, by, BUTTON_W, StatusOverlay::CTRL_H),
         Rect::new(x + BUTTON_W + BUTTON_GAP, by, BUTTON_W, StatusOverlay::CTRL_H))
    };
    Layout {
        panel,
        question: Rect::new(panel.x + PAD_X, panel.y + PAD_TOP, BODY_W, question_h),
        body: Rect::new(panel.x + PAD_X, panel.y + PAD_TOP + question_h + BODY_GAP, BODY_W, body_h),
        cancel,
        destructive,
        text_viewport: Rect::new(panel.x + PAD_X, panel.y + PAD_TOP, BODY_W, viewport_h),
        scroll_max: if overflow { text_h + theme::space::XS - viewport_h } else { 0.0 },
    }
}

pub(crate) struct DecisionAlert {
    pop: Popover,
    choice: Choice,
    controls: CtlPop<2>,
    /// Stored with the body so draw and pointer hit-testing use the same measured wrapping even on
    /// the first frame after open.
    question: &'static str,
    /// The optional paragraphs under the question, held on the ALERT rather than passed to
    /// [`draw`](Self::draw). It has to be here because the body changes where the buttons are, and
    /// [`frames`](Self::frames) must compute the same panel as the last draw did. A body passed
    /// per-frame would be correct for drawing and wrong for the first hit test after an open, which
    /// is precisely the frame a fast click lands on.
    ///
    /// Owned-or-static paragraphs, stacked [`PARA_GAP`] apart. A card whose body names a runtime
    /// value (a Report ID) cannot be a `&'static str`. Empty = no body.
    body: Vec<Cow<'static, str>>,
    scroll: Spring,
    scroll_target: f32,
    /// One answer or two — see [`Answers`].
    answers: Answers,
    /// Which face the destructive-slot answer wears — see [`Tone`]. Defaults to
    /// [`Tone::Destructive`], so every alert built before this field existed looks exactly as it
    /// did.
    tone: Tone,
    /// The panel's material: the page under the alert, latched ONCE per open from the framebuffer
    /// at the head of [`draw_scrim`](Self::draw_scrim), before the alert's own dim touches it —
    /// the same field `containers::modal::ModalUnderlay` latches for a surface, owned here because
    /// this alert is drawn from inside its page rather than as a `ModalStack` surface, so no
    /// container field describes what is actually behind it (over Privacy & data, the stack's
    /// field is HOME's, one surface further down). Over the video plane (the player's repair
    /// alert) the sample refuses and the panel draws the flat sheet — what the old glass did there.
    field: crate::ui::underlay::UnderlayField,
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
            question: "",
            body: Vec::new(),
            scroll: Spring::at(0.0),
            scroll_target: 0.0,
            answers: Answers::Two,
            tone: Tone::Destructive,
            field: crate::ui::underlay::UnderlayField::new(),
        }
    }
    /// Set the second answer's face. Called once, outside the draw loop — a tone is a property of
    /// what the alert is FOR, not something that changes frame to frame.
    pub(crate) fn set_tone(&mut self, tone: Tone) {
        self.tone = tone;
    }
    pub(crate) fn is_open(&self) -> bool {
        self.pop.is_open()
    }
    /// **Has the sheet finished arriving?** A key acts on the logical state, but a POINTER hit is
    /// positional and [`Self::frames`] tests FINAL coordinates while the sheet is still drawn
    /// through the entrance painter — so an immediate click lands on a control that is displaced
    /// and nearly invisible. Every pointer caller gates on this (`ui::route_screen`'s rule 11).
    pub(crate) fn settled(&self) -> bool {
        self.pop.appear_settled()
    }
    /// Ask the question alone.
    pub(crate) fn open(&mut self, question: &'static core::ffi::CStr) {
        self.question = question.to_str().unwrap_or("");
        self.body.clear();
        self.answers = Answers::Two;
        self.open_inner();
    }
    /// Ask it with a paragraph underneath — for a question whose consequences are not fully stated
    /// by its verb. The destructive delete is the case: "Delete all local data" is accurate about
    /// what it removes and silent about what it CANNOT reach, and the confirmation is the last
    /// moment that difference can be stated.
    pub(crate) fn open_with_body(
        &mut self,
        question: &'static core::ffi::CStr,
        body: &'static str,
    ) {
        self.open_card(question, vec![Cow::Borrowed(body)], Answers::Two);
    }
    /// Open as a CARD: a title, any number of body paragraphs (each wrapped on its own, stacked
    /// [`PARA_GAP`] apart) and one or two answers. Focus starts on the cancel slot like every open;
    /// a caller whose default is the second answer says so with [`set_choice`](Self::set_choice).
    pub(crate) fn open_card(
        &mut self,
        question: &'static core::ffi::CStr,
        paragraphs: Vec<Cow<'static, str>>,
        answers: Answers,
    ) {
        self.open(question);
        self.reconcile_card(question, paragraphs, answers);
    }
    /// Reconcile the content an OPEN card renders, without reopening or resetting its spring.
    /// Equality is over the title, paragraphs and answers, not an owner's incident ID or state
    /// tag: a receipt can arrive while both of those stay the same. Returns whether paint/layout
    /// changed. Closed and dismissing cards keep their last content for the exit choreography.
    pub(crate) fn reconcile_card(
        &mut self,
        question: &'static core::ffi::CStr,
        paragraphs: Vec<Cow<'static, str>>,
        answers: Answers,
    ) -> bool {
        let question = question.to_str().unwrap_or("");
        if !self.is_open()
            || (self.question == question && self.body == paragraphs && self.answers == answers)
        {
            return false;
        }
        let _own = crate::ui::popover::own_motion();
        self.question = question;
        self.body = paragraphs;
        self.answers = answers;
        self.choice = self.valid_choice(self.choice);
        // The body determines the panel's size. A cached ground can contain the OLD outline;
        // dropping it here lets all hosts redraw the measured panel, even inside an own scope.
        crate::ui::popover::host::ground_invalidate();
        crate::ui::idle::invalidate();
        true
    }
    /// One answer or two, as last opened or reconciled.
    pub(crate) fn answers(&self) -> Answers {
        self.answers
    }
    /// The retained paragraphs DRAW consumes, rather than the caller's latest proposed body.
    #[cfg(test)]
    pub(crate) fn body_for_test(&self) -> &[Cow<'static, str>] {
        &self.body
    }
    fn open_inner(&mut self) {
        self.choice = Choice::Cancel;
        self.scroll_target = 0.0;
        self.scroll.jump(0.0);
        // A fresh open is a fresh page under it: re-latch at the next `draw_scrim`.
        self.field.reset();
        self.pop.open();
        crate::ui::idle::invalidate();
    }
    /// Measure retained content through the caller's capability, before paint or during replay.
    fn measured(&self, measure: &dyn Measure) -> Layout {
        let qh = Self::question_view(self.question).with_measure(measure).measure_h(BODY_W);
        let heights: Vec<f32> = self.body.iter()
            .map(|body| Self::body_view(body).with_measure(measure).measure_h(BODY_W)).collect();
        layout_with(qh, body_h(&heights), self.answers)
    }
    /// Final answer frames, measured exactly as drawing without depending on a previous paint.
    pub(crate) fn frames(&self, measure: &dyn Measure) -> (Rect, Rect) {
        let l = self.measured(measure);
        (l.cancel, l.destructive)
    }
    /// Vertical input reads the disclosure while horizontal focus retains the answer.
    /// Return canonical target bits for the owner's logical state.
    pub(crate) fn scroll_by(&mut self, measure: &dyn Measure, delta: i32) -> u32 {
        let max = self.measured(measure).scroll_max;
        self.scroll_target = (self.scroll_target + delta as f32 * theme::size::BODY as f32 * 6.0)
            .clamp(0.0, max);
        let _own = crate::ui::popover::own_motion();
        crate::ui::idle::invalidate();
        self.scroll_target.to_bits()
    }
    pub(crate) fn scroll_target_bits(&self) -> u32 { self.scroll_target.to_bits() }

    /// Confirmation disclosures must be complete, never silently capped. The panel grows from
    /// this same measured view. Each caller still needs a simulator capture confirming that the
    /// full title, disclosure and buttons fit the viewport; measurement alone does not prove it.
    /// A character-count budget cannot prove wrapping: the richer report disclosure fit that
    /// budget but exceeded the former eight-line cap and lost its final privacy sentence.
    pub(crate) fn body_view(text: &str) -> TextView<'_> {
        TextView::new(text, theme::size::BODY, theme::TEXT_READING)
            .h(theme::alert::TEXT_ALIGN)
            .break_long_words()
    }
    pub(crate) fn question_view(text: &str) -> TextView<'_> {
        TextView::new(text, theme::size::TITLE, theme::TEXT_PRIMARY)
            .bold()
            .h(theme::alert::TEXT_ALIGN)
            .break_long_words()
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
    fn valid_choice(&self, choice: Choice) -> Choice {
        if self.answers == Answers::One { Choice::Cancel } else { choice }
    }
    pub(crate) fn set_choice(&mut self, choice: Choice) {
        self.choice = self.valid_choice(choice);
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
        self.scroll.step(self.scroll_target, K_SCROLL, dt);
        self.controls.step(
            Some(matches!(self.choice, Choice::Destructive) as usize),
            dt,
        );
    }
    pub(crate) fn draw_scrim(&mut self) {
        if self.visible() {
            // Live over the frozen host — and the first `live` of a frame is what takes the
            // snapshot, before this scrim lands on it. See `popover::host::live`.
            let _live = crate::ui::popover::host::live();
            // The panel's field, from the page as it stands before the dim below touches it —
            // idempotent once latched; until the read lands, and on a refusal (the video plane),
            // the flat sheet.
            let _ = self.field.latch_from_frame(
                crate::ui::underlay::Grade::Dim,
                crate::ui::popover::host::page_tex(),
            );
            // The DECISION role's weight, as the flat ink (`Popover::scrim`): this alert is a
            // `Popover` drawn from inside its page (consent, and the player's repair alert over the
            // video plane), not a `ModalStack` surface, so its DIM has no container field to inherit.
            self.pop.scrim(theme::underlay::DIM_DECISION);
        }
    }
    pub(crate) fn draw(
        &mut self,
        cancel: &core::ffi::CStr,
        destructive: &core::ffi::CStr,
        measure: &dyn Measure,
    ) {
        if !self.visible() {
            return;
        }
        // Everything below is LIVE over the frozen host page — see `popover::host::live`.
        let _live = crate::ui::popover::host::live();
        let l = self.measured(measure);
        let p = self.pop.content_painter(Popover::RISE);
        crate::ui::profile::phase("da.panel", || self.pop.panel(p, l.panel, theme::ALERT_PANEL_RAD, Some(&self.field)));
        crate::ui::profile::phase("da.text", || {
            let scroll = self.scroll.pos.clamp(0.0, l.scroll_max);
            if l.scroll_max > 0.0 { p.clip(l.text_viewport); }
            let text_painter = p.translate(0.0, -scroll);
            Self::question_view(self.question).with_measure(measure).draw(text_painter, l.question);
            let mut y = l.body.y;
            for para in &self.body {
                let view = Self::body_view(para).with_measure(measure);
                let h = view.measure_h(BODY_W);
                view.draw(text_painter, Rect::new(l.body.x, y, l.body.w, h));
                y += h + PARA_GAP;
            }
            if l.scroll_max > 0.0 {
                p.clip_clear();
                crate::ui::widgets::continuous_scroll_rail(p,
                    Rect::new(l.text_viewport.x + l.text_viewport.w + theme::space::SM,
                        l.text_viewport.y, crate::ui::widgets::RAIL_W, l.text_viewport.h),
                    scroll, l.text_viewport.h + l.scroll_max, l.text_viewport.h);
            }
        });
        let env = Env::inert();
        crate::ui::profile::phase("da.ctl", || {
        Button::new(cancel.as_ptr(), theme::size::BODY, l.cancel)
            .focused(self.choice == Choice::Cancel)
            .scale(self.controls.scale(0))
            .draw(&env, p);
        if self.answers == Answers::One {
            return;
        }
        let destructive_style = match self.tone {
            Tone::Destructive => ControlStyle::Danger,
            Tone::Neutral => ControlStyle::Accent,
        };
        Button::new(destructive.as_ptr(), theme::size::BODY, l.destructive)
            .style(destructive_style)
            .focused(self.choice == Choice::Destructive)
            .scale(self.controls.scale(1))
            .draw(&env, p);
        });
    }
}

/// The body block's height from each paragraph's measured height: stacked [`PARA_GAP`] apart, and
/// 0.0 — the no-body geometry — when there are none.
fn body_h(heights: &[f32]) -> f32 {
    if heights.is_empty() {
        return 0.0;
    }
    heights.iter().sum::<f32>() + PARA_GAP * (heights.len() - 1) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unicode scalar metrics avoid treating every Belarusian glyph as two UTF-8 bytes. Real
    /// font/cap-band rendering is verified separately in the simulator and on the television.
    struct DisclosureMeasure;
    impl Measure for DisclosureMeasure {
        fn width(&self, text: &core::ffi::CStr, size: i32, _bold: bool) -> f32 {
            text.to_string_lossy().chars().count() as f32 * size as f32 * 0.58
        }
        fn cap_h(&self, size: i32) -> f32 { size as f32 * 0.72 }
        fn line_h(&self, size: i32) -> f32 { size as f32 * 1.32 }
    }

    #[test]
    fn belarusian_delete_question_and_complete_scope_wrap_at_the_existing_text_sizes() {
        let _guard = plx_base::testlock::serial();
        let measure = DisclosureMeasure;
        let locale = crate::i18n::LocaleContext::resolve(crate::i18n::Preference::Be, None, None, None, None);
        let question = crate::i18n::msg::settings_consent_delete_question_in(&locale);
        let body = crate::i18n::msg::settings_consent_delete_scope_in(&locale);
        assert!(body.contains("серверы Plex") && body.contains("Sentry") && body.contains("PostHog"));
        let question_view = DecisionAlert::question_view(question).with_measure(&measure);
        let body_view = DecisionAlert::body_view(body).with_measure(&measure);
        assert!(!question_view.truncates(BODY_W));
        assert!(!body_view.truncates(BODY_W), "the old four-line limit hid the deletion exclusions");
        assert!(question_view.measure_h(BODY_W) > measure.line_h(theme::size::TITLE));
        assert!(body_view.measure_h(BODY_W) > 4.0 * measure.line_h(theme::size::BODY));
        let mut alert = DecisionAlert::new();
        alert.open_with_body(crate::i18n::msg::settings_consent_delete_question_c_in(&locale), body);
        let l = alert.measured(&measure);
        assert_eq!(alert.choice(), Choice::Cancel);
        assert_eq!(l.question.w, 564.0);
        assert_eq!(l.question.x - l.panel.x, 48.0);
        assert_eq!(l.body.h, body_view.measure_h(BODY_W));
        assert_eq!(l.scroll_max, 0.0, "the real translation fits without hiding any scope");
        let (cancel, destructive) = alert.frames(&measure);
        assert_eq!((cancel.x, cancel.y, cancel.w, cancel.h), (l.cancel.x, l.cancel.y, l.cancel.w, l.cancel.h));
        assert_eq!(cancel.w, destructive.w);
        assert!(l.body.y + l.body.h < cancel.y);
        assert!(l.panel.y >= SAFE.y && l.panel.y + l.panel.h <= SAFE.y + SAFE.h);
    }

    #[test]
    fn oversized_disclosure_scrolls_to_its_end_while_both_answers_stay_inside_the_safe_area() {
        let _guard = plx_base::testlock::serial();
        let measure = DisclosureMeasure;
        let mut alert = DecisionAlert::new();
        alert.open_card(c"A future translated question", vec![
            "A future translated disclosure with a complete scope ".repeat(100).into()
        ], Answers::Two);
        let l = alert.measured(&measure);
        assert!(l.scroll_max > 0.0);
        assert_eq!(l.panel.h, SAFE.h);
        assert!(l.cancel.y >= l.text_viewport.y + l.text_viewport.h);
        assert!(l.cancel.y + l.cancel.h <= SAFE.y + SAFE.h);
        assert_eq!(l.cancel.w, l.destructive.w);
        assert_ne!(alert.scroll_by(&measure, 1), 0);
        alert.scroll_by(&measure, i32::MAX);
        assert_eq!(alert.scroll_target, l.scroll_max);
        assert!(l.body.y + l.body.h - alert.scroll_target < l.text_viewport.y + l.text_viewport.h,
            "even the final scope sentence remains reachable");
        assert_eq!(alert.choice(), Choice::Cancel, "reading never selects the destructive answer");
        alert.scroll_by(&measure, i32::MIN);
        assert_eq!(alert.scroll_target, 0.0);
    }


    #[test]
    fn reconciling_a_card_preserves_motion_and_valid_focus_and_is_quiet_when_unchanged() {
        let _serial = plx_base::testlock::serial();
        let mut alert = DecisionAlert::new();
        alert.open_card(c"Details", vec!["old".into()], Answers::Two);
        alert.set_choice(Choice::Destructive);
        for _ in 0..90 { alert.update(1.0 / 60.0); }
        assert!(alert.settled());
        let appear = alert.pop.appear();
        let users = crate::ui::popover::host_users_for_test();
        assert!(alert.reconcile_card(c"Details", vec!["receipt".into()], Answers::Two));
        assert_eq!(alert.body_for_test(), ["receipt"]);
        assert_eq!(alert.choice(), Choice::Destructive);
        assert_eq!(alert.pop.appear(), appear, "content must not restart the entrance");
        assert_eq!(crate::ui::popover::host_users_for_test(), users);
        crate::ui::idle::take_local_damage();
        assert!(!alert.reconcile_card(c"Details", vec!["receipt".into()], Answers::Two));
        assert_eq!(crate::ui::idle::take_local_damage(), 0, "a settled card must stay idle");
        assert!(alert.reconcile_card(c"Details", vec!["receipt".into()], Answers::One));
        assert_eq!(alert.choice(), Choice::Cancel, "the removed answer hands focus to Close");
        alert.set_choice(Choice::Destructive);
        assert_eq!(alert.choice(), Choice::Cancel, "a late focus delivery cannot select a missing answer");
        alert.dismiss();
        assert!(!alert.reconcile_card(c"Details", vec!["new".into()], Answers::Two));
        assert_eq!(alert.body_for_test(), ["receipt"], "the exit keeps its last picture");
    }
    #[test]
    fn owned_multi_paragraph_card_scrolls_with_one_visible_answer_without_live_measurement() {
        let _serial = plx_base::testlock::serial();
        let _no_live = crate::ui::text_view::ForbidLive::enter();
        let measure = DisclosureMeasure;
        let mut alert = DecisionAlert::new();
        alert.open_card(c"Report details", vec![
            "Long translated report disclosure. ".repeat(120).into(),
            "Final retained receipt and support details.".into(),
        ], Answers::One);
        let l = alert.measured(&measure);
        let frames = alert.frames(&measure);
        assert_eq!(frames.1.w, 0.0);
        assert_eq!(frames.0.cx(), l.panel.cx());
        assert_eq!(l.panel.h, SAFE.h);
        alert.scroll_by(&measure, i32::MAX);
        assert_eq!(f32::from_bits(alert.scroll_target_bits()), l.scroll_max);
        assert!(l.body.y + l.body.h - alert.scroll_target < l.text_viewport.y + l.text_viewport.h);
        assert_eq!(alert.choice(), Choice::Cancel);
        assert_eq!(alert.body_for_test()[1], "Final retained receipt and support details.");
        alert.close();
    }

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

    /// **The tone defaults to `Destructive`**, so a caller that never touches it — every alert
    /// this app shipped before issue #75 — looks exactly as it always did.
    #[test]
    fn tone_defaults_to_destructive_and_the_setter_changes_it() {
        let mut a = DecisionAlert::new();
        assert_eq!(a.tone, Tone::Destructive);
        a.set_tone(Tone::Neutral);
        assert_eq!(a.tone, Tone::Neutral);
    }

    #[test]
    fn two_actions_share_one_measured_row_and_cancel_is_first() {
        let l = layout(40.0, 0.0);
        assert_eq!(l.cancel.w, l.destructive.w);
        assert_eq!(l.cancel.y, l.destructive.y);
        assert!(l.cancel.x < l.destructive.x);
        assert!(l.question.y + l.question.h < l.cancel.y);
    }

    #[test]
    fn a_wrapped_question_grows_the_panel_and_keeps_hit_geometry_below_it() {
        let short = layout(48.0, 0.0);
        let long = layout(96.0, 0.0);

        assert!(matches!(theme::alert::TEXT_ALIGN, crate::ui::label::HAlign::Left));
        assert!(long.question.h > short.question.h, "the long title must wrap, never overflow");
        let growth = long.question.h - short.question.h;
        assert_eq!(long.panel.h - short.panel.h, growth);
        assert_eq!(
            (long.cancel.y - long.panel.y) - (short.cancel.y - short.panel.y),
            growth,
            "draw and pointer hit rectangles move by the measured wrap height"
        );
    }

    #[test]
    fn reopening_with_a_question_alone_clears_the_previous_body() {
        let _serial = plx_base::testlock::serial();
        let mut alert = DecisionAlert::new();
        alert.open_with_body(c"First question?", "Consequences of the first question.");
        assert!(!alert.body.is_empty());
        alert.open(c"Second question?");
        assert_eq!(alert.question, "Second question?");
        assert!(alert.body.is_empty());
        alert.close();
    }

    /// **A one-answer card centres its one answer** in the cancel slot, keeps the two-answer
    /// panel's height, and a re-open as a question gets both answers back.
    #[test]
    fn a_one_answer_card_centres_its_answer_in_the_cancel_slot() {
        let two = layout(40.0, 90.0);
        let one = layout_with(40.0, 90.0, Answers::One);
        assert_eq!((one.panel.y, one.panel.h), (two.panel.y, two.panel.h));
        assert_eq!(one.cancel.w, BUTTON_W);
        assert!((one.cancel.cx() - one.panel.cx()).abs() < 1e-3);
        assert_eq!(one.cancel.y, two.cancel.y);
        assert_eq!(one.destructive.w, 0.0);
        let _serial = plx_base::testlock::serial();
        let mut alert = DecisionAlert::new();
        alert.open_card(c"Details", vec!["A".into(), "B".into()], Answers::One);
        assert_eq!((alert.answers(), alert.body.len()), (Answers::One, 2));
        alert.open(c"Question?");
        assert_eq!(alert.answers(), Answers::Two);
        alert.close();
    }

    /// Paragraphs stack [`PARA_GAP`] apart, and none is the no-body geometry.
    #[test]
    fn paragraphs_stack_one_rung_apart() {
        assert_eq!(body_h(&[]), 0.0);
        assert_eq!(body_h(&[30.0]), 30.0);
        assert_eq!(body_h(&[30.0, 60.0]), 90.0 + PARA_GAP);
    }
}
