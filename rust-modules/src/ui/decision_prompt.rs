//! Engine focus and input adapter for a reusable two-answer decision alert.
//! The host consumes a boolean answer; no application command or persistence lives here.
use std::ffi::CStr;
use crate::ui::decision_alert::{Choice, DecisionAlert, Tone};
use crate::ui::machine::{Cx, Edge, EntryId, FocusKey, GroupId, Handled, Host, InputEvent, InputKind, Key};
use crate::ui::screen::{Activate, AxisMask, Dir, DrawFrame, EdgeRule, ElemKind, GroupKind, GroupSpec, Hover, Placed, ScreenEvent, Seat, Step, Stop};
use crate::ui::{Painter, Rect};

pub(crate) enum PromptStep { Pass, Done(Handled), Answer(bool) }

pub(crate) struct DecisionPrompt {
    alert: DecisionAlert,
    group: GroupId,
    cancel: u32,
    affirm: u32,
    cancel_label: &'static CStr,
    affirm_label: &'static CStr,
}
impl DecisionPrompt {
    pub(crate) fn new(group: GroupId, cancel: u32, affirm: u32,
        cancel_label: &'static CStr, affirm_label: &'static CStr) -> Self {
        Self { alert: DecisionAlert::new(), group, cancel, affirm, cancel_label,
            affirm_label }
    }
    pub(crate) fn open(&mut self, question: &'static CStr, body: &'static str) {
        self.alert.set_tone(Tone::Neutral);
        self.alert.open_with_body(question, body);
    }
    pub(crate) fn is_open(&self) -> bool { self.alert.is_open() }
    pub(crate) fn visible(&self) -> bool { self.alert.visible() }
    pub(crate) fn owns(&self, elem: u32) -> bool { elem == self.cancel || elem == self.affirm }
    pub(crate) fn choice(&self) -> bool { self.alert.choice() == Choice::Destructive }
    pub(crate) fn scroll_target_bits(&self) -> u32 { self.alert.scroll_target_bits() }
    pub(crate) fn update(&mut self, dt: f32) { self.alert.update(dt); }
    pub(crate) fn step<H: Host<Elem=u32>>(&mut self, ev: &ScreenEvent<H>, cx: &Cx<'_, H>) -> PromptStep {
        match ev {
            ScreenEvent::FocusMoved { to, .. } if self.is_open() && self.owns(to.elem) => {
                self.alert.set_choice(if to.elem == self.affirm { Choice::Destructive } else { Choice::Cancel });
                PromptStep::Done(Handled::Yes)
            }
            ScreenEvent::Activate(_) if self.visible() => PromptStep::Done(Handled::Yes),
            ScreenEvent::PressCommit(_) => {
                match cx.focus.current.map(|k| k.elem) {
                    Some(elem) if self.is_open() && self.owns(elem) => {
                        self.alert.dismiss(); PromptStep::Answer(elem == self.affirm)
                    }
                    Some(elem) if self.owns(elem) => PromptStep::Done(Handled::Yes),
                    _ if self.visible() => PromptStep::Done(Handled::Yes),
                    _ => PromptStep::Pass,
                }
            }
            ScreenEvent::Input(InputEvent { kind: InputKind::Key { key, edge, .. }, .. }) if self.visible() => {
                if !self.is_open() { return PromptStep::Done(Handled::Yes); }
                match key {
                    Key::Back if *edge == Edge::Down => {
                        self.alert.dismiss(); PromptStep::Answer(false)
                    }
                    Key::Up | Key::Down if *edge != Edge::Up => {
                        self.alert.scroll_by(cx.measure, if *key == Key::Up { -1 } else { 1 });
                        PromptStep::Done(Handled::Yes)
                    }
                    Key::Back | Key::Up | Key::Down => PromptStep::Done(Handled::Yes),
                    _ => PromptStep::Done(Handled::No),
                }
            }
            _ => PromptStep::Pass,
        }
    }
    fn key(&self, entry: EntryId, elem: u32) -> FocusKey<u32> {
        FocusKey { entry, elem }
    }

    fn rect(&self, elem: u32, measure: &dyn crate::ui::machine::Measure) -> Rect {
        let (cancel, affirm) = self.alert.frames(measure);
        if elem == self.affirm { affirm } else { cancel }
    }

    /// `Focusable::groups`: while open, the answers are the ONLY group — push it and return
    /// `true` so the host adds none of its own.
    pub(crate) fn groups(&self, out: &mut Vec<GroupSpec>, measure: &dyn crate::ui::machine::Measure) -> bool {
        if !self.is_open() {
            return false;
        }
        out.push(GroupSpec {
            id: self.group,
            kind: GroupKind::Row { wrap: false },
            seat: Seat::First,
            reachable: AxisMask::BOTH,
            edge: [EdgeRule::Stop; 4],
            extent: self.rect(self.cancel, measure).union(self.rect(self.affirm, measure)),
            len: 2,
            elem: ElemKind::Control,
        });
        true
    }

    /// `Focusable::group_of`: `Some(answer)` while open — `Some(None)` for a key that is not an
    /// answer, which the open alert makes unfocusable — and `None` when closed (the host's).
    pub(crate) fn group_of(&self, key: u32) -> Option<Option<GroupId>> {
        self.is_open().then(|| self.owns(key).then_some(self.group))
    }

    /// `Focusable::neighbour` while open: LEFT/RIGHT between the two answers, an edge otherwise.
    pub(crate) fn neighbour(&self, key: FocusKey<u32>, dir: Dir) -> Option<Step<u32>> {
        if !self.is_open() {
            return None;
        }
        Some(match (key.elem, dir) {
            (e, Dir::Right) if e == self.cancel => Step::Move(self.key(key.entry, self.affirm)),
            (e, Dir::Left) if e == self.affirm => Step::Move(self.key(key.entry, self.cancel)),
            _ => Step::Edge,
        })
    }

    /// `Focusable::place` while open.
    pub(crate) fn place(&self, key: u32, measure: &dyn crate::ui::machine::Measure) -> Option<Option<Placed>> {
        if !self.is_open() {
            return None;
        }
        Some(self.owns(key).then(|| {
            let rect = self.rect(key, measure);
            Placed { rect, rest_rect: rect, clip: Rect::FULL, index: Some((key == self.affirm) as u32) }
        }))
    }

    /// `Focusable::reconcile` while open: a stray key bounces back to the answer the alert
    /// already stands on.
    pub(crate) fn reconcile(&self, want: FocusKey<u32>) -> Option<FocusKey<u32>> {
        if !self.is_open() {
            return None;
        }
        if self.owns(want.elem) {
            return Some(want);
        }
        Some(self.key(want.entry, match self.alert.choice() {
            Choice::Cancel => self.cancel,
            Choice::Destructive => self.affirm,
        }))
    }

    /// `Focusable::seat` for the answers' group: the cancel action.
    pub(crate) fn seat(&self, g: GroupId, entry: EntryId) -> Option<FocusKey<u32>> {
        (g == self.group).then(|| self.key(entry, self.cancel))
    }

    /// Draw the alert over the host's page, and register its answers' stops once it has settled.
    pub(crate) fn draw<H: Host<Elem = u32>>(&mut self, f: &mut DrawFrame<'_, '_, H>, entry: EntryId) {
        if !self.alert.visible() {
            return;
        }
        self.alert.draw_scrim();
        let (cancel, affirm) = (self.cancel_label, self.affirm_label);
        self.alert.draw(cancel, affirm, f.measure);
        let frames = self.alert.frames(f.measure);
        if self.alert.is_open() && self.alert.settled() {
            for (elem, rect) in [(self.cancel, frames.0), (self.affirm, frames.1)] {
                f.stop(
                    Painter::root(),
                    Stop {
                        key: self.key(entry, elem),
                        rect,
                        rest_rect: rect,
                        clip: Rect::FULL,
                        hover: Hover::Focus,
                        activate: Activate::Press,
                    },
                );
            }
        }
    }
}
