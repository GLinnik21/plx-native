//! Reusable two-choice decision alert.  It owns focus, hit geometry and destructive styling; a
//! caller supplies only the question and the two verbs.

use crate::ui::label::{HAlign, Label};
use crate::ui::popover::Popover;
use crate::ui::widgets::{Button, ControlStyle, CtlPop, StatusOverlay};
use crate::ui::{theme, Env, Rect, View};

const PANEL_W: f32 = 660.0;
const PAD_TOP: f32 = theme::alert::PAD;
const PAD_X: f32 = theme::alert::PAD;
const PAD_BOTTOM: f32 = 34.0;
const QUESTION_GAP: f32 = theme::space::LG;
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
    pub(crate) cancel: Rect,
    pub(crate) destructive: Rect,
}

pub(crate) fn layout(question_h: f32) -> Layout {
    let h = PAD_TOP + question_h + QUESTION_GAP + StatusOverlay::CTRL_H + PAD_BOTTOM;
    let panel = Rect::new(
        (Rect::FULL.w - PANEL_W) * 0.5,
        (Rect::FULL.h - h) * 0.5,
        PANEL_W,
        h,
    );
    let row_w = BUTTON_W * 2.0 + BUTTON_GAP;
    let x = panel.cx() - row_w * 0.5;
    let by = panel.y + PAD_TOP + question_h + QUESTION_GAP;
    Layout {
        panel,
        question: Rect::new(
            panel.x + PAD_X,
            panel.y + PAD_TOP,
            panel.w - 2.0 * PAD_X,
            question_h,
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
}

impl DecisionAlert {
    pub(crate) const fn new() -> Self {
        Self {
            pop: Popover::new(),
            choice: Choice::Cancel,
            controls: CtlPop::new(),
        }
    }
    pub(crate) fn is_open(&self) -> bool {
        self.pop.is_open()
    }
    pub(crate) fn open(&mut self) {
        self.choice = Choice::Cancel;
        self.pop.open();
        crate::ui::idle::invalidate();
    }
    pub(crate) fn close(&mut self) {
        self.pop.close();
        crate::ui::idle::invalidate();
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
        if !self.is_open() {
            return;
        }
        self.pop.update(dt);
        self.controls.step(
            Some(matches!(self.choice, Choice::Destructive) as usize),
            dt,
        );
    }
    pub(crate) fn draw_scrim(&self) {
        if self.is_open() {
            self.pop.scrim(0.55);
        }
    }
    pub(crate) fn draw(
        &mut self,
        question: &core::ffi::CStr,
        cancel: &core::ffi::CStr,
        destructive: &core::ffi::CStr,
    ) {
        if !self.is_open() {
            return;
        }
        let qh = crate::text::text_height(theme::size::TITLE, 1);
        let l = layout(qh);
        let p = self.pop.content_painter(Popover::RISE);
        self.pop.panel(p, l.panel, theme::ALERT_PANEL_RAD);
        Label::new(question.as_ptr(), theme::size::TITLE, theme::TEXT_PRIMARY)
            .bold()
            .h(HAlign::Center)
            .draw(p, l.question);
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
        let l = layout(crate::text::text_height(theme::size::TITLE, 1));
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
    #[test]
    fn two_actions_share_one_measured_row_and_cancel_is_first() {
        let l = layout(40.0);
        assert_eq!(l.cancel.w, l.destructive.w);
        assert_eq!(l.cancel.y, l.destructive.y);
        assert!(l.cancel.x < l.destructive.x);
        assert!(l.question.y + l.question.h < l.cancel.y);
    }
}
