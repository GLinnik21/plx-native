//! **"Connect without encryption?" — the one question, and every surface that asks it**
//! (PLX-NATIVE-10).
//!
//! Four surfaces ask it: the sign-in read-out, Home's and a Library source's failure read-out
//! (when the server that failed is one discovery OFFERED — `plex::grant::offers`), and Settings'
//! *Unencrypted connections* switch turned on. They share this module rather than a copy each:
//! the question and its body, the two verbs (seated on *Not now*), which primary a verdict earns
//! on a read-out ([`asks`]), the reason line under it ([`reason`]), and the ONE command an answer
//! becomes ([`PlaintextQuestion::answer`] → `auth::SessionCmd::AnswerPlaintext`). The Session
//! owner records it for the signed-in account and, on *Connect*, re-finds the server so a fresh
//! probe can mint the grant (`plex::grant`).
//!
//! The sign-in screen hosts the question on its own alert (it shares that sheet with the report
//! question and the Details card). Every other surface holds a [`PlaintextAlert`] — the question
//! with its own [`DecisionAlert`] and the focus group its two answers form, which traps focus
//! while it is open, as every decision alert does — and delegates its focus, pointer and draw
//! hooks to it.
//!
//! Not a screen: shared plumbing, named through `super::` like `family`, so no screen reaches a
//! sibling to ask it.

use std::cell::Cell;
use std::ffi::{CStr, CString};

use crate::auth::{self, PlaintextVerdict, ReadoutSurface, SessionCmd};
use crate::plex::session::PlaintextChoice;
use crate::plex::ServerId;
use crate::ui::decision_alert::{Choice, DecisionAlert, Tone};
use crate::ui::machine::{
    Cx, Delivery, Edge, Effects, EntryId, FocusKey, Fx, GroupId, Handled, Host, InputEvent,
    InputKind, Key, MachineId,
};
use crate::ui::screen::{
    Activate, AxisMask, Dir, DrawFrame, EdgeRule, ElemKind, Enter, FocusTarget, GroupKind,
    GroupSpec, Hover, Placed, ScreenEvent, Seat, Step, Stop,
};
use crate::ui::{Painter, Rect};

/// The read-out's primary while the question can be asked — and the question's affirmative verb,
/// the same word, so the reason's "Select Connect" names the button either way.
pub(crate) const CONNECT: &CStr = c"Connect";
/// The read-out's primary once the person has answered (or on any other failure).
pub(crate) const TRY_AGAIN: &CStr = c"Try again";
pub(crate) const NOT_NOW: &CStr = c"Not now";
/// The question — a choice, not a failure report.
pub(crate) const QUESTION: &CStr = c"Connect without encryption?";
/// Its one line: what saying yes does, in the words a person uses. True of `crate::plex::grant`:
/// the sign-in travels only to the one verified address, on this network.
pub(crate) const BODY: &str =
    "Your Plex sign-in will travel without encryption over this network, to this server only.";

/// **Does a read-out about `verdict` ask the question?** Only for a server the same-network rule
/// lets the person be asked about (`PlaintextVerdict::offers`) that they have NOT answered: once
/// they said *Not now* (or turned it off in Settings) the primary is *Try again* and the reason
/// says where to allow it — a question already answered is not put again from a failure.
pub(crate) fn asks(verdict: Option<&PlaintextVerdict>) -> bool {
    verdict.is_some_and(|v| v.offers() && v.choice == PlaintextChoice::Undecided)
}

/// The read-out's primary for `verdict`: [`CONNECT`] when it [`asks`], [`TRY_AGAIN`] otherwise.
pub(crate) fn primary(verdict: Option<&PlaintextVerdict>) -> &'static CStr {
    if asks(verdict) { CONNECT } else { TRY_AGAIN }
}

/// The reason line a SIGNED-IN read-out (Home, a Library source) shows for `verdict` — the one
/// copy table (`auth::plaintext_copy`), as a C string for the status overlay.
pub(crate) fn reason(verdict: &PlaintextVerdict) -> CString {
    CString::new(auth::plaintext_copy(Some(verdict), ReadoutSurface::SignedIn).into_owned())
        .unwrap_or_default()
}

/// Who the open question is about: the server, and the slot to re-find on *Connect* (`None` when
/// it has none — the whole roster is re-found instead).
#[derive(Clone, Debug, PartialEq, Eq)]
struct Subject {
    machine_id: String,
    sid: Option<ServerId>,
}

/// **The question itself**, over an alert its host owns: opening it with the shared copy, and
/// turning the answer into the one command. The sign-in screen uses this directly on its shared
/// sheet; [`PlaintextAlert`] wraps it for every other surface.
#[derive(Clone, Debug, Default)]
pub(crate) struct PlaintextQuestion {
    subject: Option<Subject>,
}

impl PlaintextQuestion {
    pub(crate) fn new() -> Self {
        Self { subject: None }
    }

    /// The server the question is open about, if it is.
    pub(crate) fn subject(&self) -> Option<&str> {
        self.subject.as_ref().map(|s| s.machine_id.as_str())
    }

    /// Ask about `machine_id` on `alert` — seated on *Not now* (every `DecisionAlert` open is), so
    /// an OK held through the press that opened it never answers yes.
    pub(crate) fn open(&mut self, alert: &mut DecisionAlert, machine_id: &str, sid: Option<ServerId>) {
        alert.set_tone(Tone::Neutral);
        alert.open_with_body(QUESTION, BODY);
        self.subject = Some(Subject { machine_id: machine_id.to_owned(), sid });
    }

    /// The two verbs the host draws the alert with: *Not now* / *Connect*.
    pub(crate) fn verbs() -> (&'static CStr, &'static CStr) {
        (NOT_NOW, CONNECT)
    }

    /// **The person answered** — *Connect* (`allow`) or *Not now* / BACK. Dismisses the alert (an
    /// answer is never an instant hide) and returns the command to send; `None` when nothing was
    /// being asked.
    pub(crate) fn answer(&mut self, alert: &mut DecisionAlert, allow: bool) -> Option<SessionCmd> {
        alert.dismiss();
        let subject = self.subject.take()?;
        crate::log(if allow {
            "plaintext: user allowed an unencrypted connection on this network"
        } else {
            "plaintext: user declined an unencrypted connection"
        });
        Some(SessionCmd::AnswerPlaintext {
            machine_id: subject.machine_id,
            choice: if allow { PlaintextChoice::Allowed } else { PlaintextChoice::Declined },
            sid: subject.sid,
        })
    }

    /// The subject went away under the open question (the offer was withdrawn): take it down
    /// without an answer.
    pub(crate) fn withdraw(&mut self, alert: &mut DecisionAlert) {
        if self.subject.take().is_some() && alert.is_open() {
            alert.close();
        }
    }
}

/// Hand the engine's focus to `group` on the screen `to` — how a surface seats focus on the
/// question's answers when it opens, and back on its own controls after an answer. A top-level
/// screen passes `fx.from()`; a Settings page passes the surface's instance, which owns the
/// family's focus.
pub(crate) fn enter_group<H: Host>(fx: &mut Effects<'_, H>, to: MachineId, group: GroupId) {
    fx.push(Fx::Deliver(
        to,
        Delivery::Screen(ScreenEvent::Enter(Enter::Fresh { focus: FocusTarget::ContainerGroup(group) })),
    ));
}

/// **The question on a surface with no alert of its own** — Home, a Library source, Settings.
///
/// It owns a [`DecisionAlert`] and the focus group its two answers form (`group`, with the host's
/// element ids `cancel` / `affirm`, chosen so they collide with nothing else the host focuses).
/// While it is OPEN those answers are the host's only group; the host's `Focusable` hooks ask
/// this first and fall through to their own when it answers `None`. It draws over whatever the
/// host drew, registers its answers as stops once settled (a pointer hit is positional —
/// `DecisionAlert::settled`), and turns an answer into the one [`SessionCmd`].
pub(crate) struct PlaintextAlert {
    alert: DecisionAlert,
    question: PlaintextQuestion,
    group: GroupId,
    cancel: u32,
    affirm: u32,
    /// The answers' rects as last drawn — the `Focusable` hooks answer with `&self`.
    frames: Cell<Option<(Rect, Rect)>>,
}

impl PlaintextAlert {
    pub(crate) fn new(group: GroupId, cancel: u32, affirm: u32) -> Self {
        Self {
            alert: DecisionAlert::new(),
            question: PlaintextQuestion::new(),
            group,
            cancel,
            affirm,
            frames: Cell::new(None),
        }
    }

    /// Open: focus belongs to the answers. The input gate.
    pub(crate) fn is_open(&self) -> bool {
        self.alert.is_open()
    }

    /// Open or still fading out — the draw (and pointer-trap) gate.
    pub(crate) fn visible(&self) -> bool {
        self.alert.visible()
    }

    /// The server the open question is about.
    pub(crate) fn subject(&self) -> Option<&str> {
        self.question.subject()
    }

    /// Is `elem` one of the answers?
    pub(crate) fn owns(&self, elem: u32) -> bool {
        elem == self.cancel || elem == self.affirm
    }

    /// Ask about `machine_id` (re-finding `sid` on *Connect*), seating focus — on the screen `to`
    /// ([`enter_group`]) — on *Not now*.
    pub(crate) fn open<H: Host>(&mut self, machine_id: &str, sid: Option<ServerId>, to: MachineId,
        fx: &mut Effects<'_, H>) {
        self.question.open(&mut self.alert, machine_id, sid);
        enter_group(fx, to, self.group);
    }

    /// Take the question down unanswered — its subject went away.
    pub(crate) fn withdraw(&mut self) {
        self.question.withdraw(&mut self.alert);
    }

    /// Step the alert's motion.
    pub(crate) fn update(&mut self, dt: f32) {
        self.alert.update(dt);
    }

    /// The engine moved focus: onto an answer, the alert's own selection follows.
    pub(crate) fn focus_moved(&mut self, elem: u32) {
        if self.is_open() && self.owns(elem) {
            self.alert.set_choice(if elem == self.affirm { Choice::Destructive } else { Choice::Cancel });
        }
    }

    /// A press on `elem`: an answer while the question is open, else `None` (not this alert's).
    /// `Some(None)` means it was an answer but nothing was being asked.
    pub(crate) fn press(&mut self, elem: u32) -> Option<Option<SessionCmd>> {
        if !self.is_open() || !self.owns(elem) {
            return None;
        }
        Some(self.question.answer(&mut self.alert, elem == self.affirm))
    }

    /// BACK while open: *Not now*. `None` when the alert is not open (BACK is the host's).
    pub(crate) fn back(&mut self) -> Option<Option<SessionCmd>> {
        if !self.is_open() {
            return None;
        }
        Some(self.question.answer(&mut self.alert, false))
    }

    /// **Route one screen event through the alert first.** While it is open the alert answers
    /// its own keys (BACK is *Not now*; UP/DOWN go nowhere), follows focus between its answers and
    /// takes the press on one; while it is still FADING OUT every key, click and commit is
    /// swallowed, so nothing under a dissolving question — the read-out's *Connect* that opened
    /// it, a Settings switch — fires through it (`screens::consent`'s delete alert has the long
    /// account of both traps). Anything else is [`AlertStep::Pass`], the host's as usual.
    pub(crate) fn step<H: Host<Elem = u32>>(&mut self, ev: &ScreenEvent<H>, cx: &Cx<'_, H>) -> AlertStep {
        match ev {
            ScreenEvent::FocusMoved { to, .. } if self.is_open() && self.owns(to.elem) => {
                self.focus_moved(to.elem);
                AlertStep::Done(Handled::Yes)
            }
            ScreenEvent::Activate(_) if self.visible() => AlertStep::Done(Handled::Yes),
            ScreenEvent::PressCommit(_) => {
                // Gate on the alert still being OPEN: a BACK queued ahead of an armed commit has
                // already answered it, and the stale focus key must not answer it twice.
                match cx.focus.current.map(|k| k.elem) {
                    Some(elem) if self.owns(elem) => match self.press(elem) {
                        Some(cmd) => AlertStep::Answer(cmd),
                        None => AlertStep::Done(Handled::Yes),
                    },
                    _ if self.visible() => AlertStep::Done(Handled::Yes),
                    _ => AlertStep::Pass,
                }
            }
            ScreenEvent::Input(InputEvent { kind: InputKind::Key { key, edge, .. }, .. })
                if self.visible() =>
            {
                if !self.is_open() {
                    return AlertStep::Done(Handled::Yes);
                }
                match key {
                    Key::Back if *edge == Edge::Down => {
                        AlertStep::Answer(self.back().flatten())
                    }
                    Key::Back | Key::Up | Key::Down => AlertStep::Done(Handled::Yes),
                    _ => AlertStep::Done(Handled::No),
                }
            }
            _ => AlertStep::Pass,
        }
    }

    fn key(&self, entry: EntryId, elem: u32) -> FocusKey<u32> {
        FocusKey { entry, elem }
    }

    fn rect(&self, elem: u32) -> Rect {
        self.frames
            .get()
            .map(|(cancel, affirm)| if elem == self.affirm { affirm } else { cancel })
            .unwrap_or(Rect::FULL)
    }

    /// `Focusable::groups`: while open, the answers are the ONLY group — push it and return
    /// `true` so the host adds none of its own.
    pub(crate) fn groups(&self, out: &mut Vec<GroupSpec>) -> bool {
        if !self.is_open() {
            return false;
        }
        out.push(GroupSpec {
            id: self.group,
            kind: GroupKind::Row { wrap: false },
            seat: Seat::First,
            reachable: AxisMask::BOTH,
            edge: [EdgeRule::Stop; 4],
            extent: self.rect(self.cancel).union(self.rect(self.affirm)),
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
    pub(crate) fn place(&self, key: u32) -> Option<Option<Placed>> {
        if !self.is_open() {
            return None;
        }
        Some(self.owns(key).then(|| {
            let rect = self.rect(key);
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

    /// `Focusable::seat` for the answers' group: *Not now*.
    pub(crate) fn seat(&self, g: GroupId, entry: EntryId) -> Option<FocusKey<u32>> {
        (g == self.group).then(|| self.key(entry, self.cancel))
    }

    /// Draw the alert over the host's page, and register its answers' stops once it has settled.
    pub(crate) fn draw<H: Host<Elem = u32>>(&mut self, f: &mut DrawFrame<'_, '_, H>, entry: EntryId) {
        if !self.alert.visible() {
            return;
        }
        self.alert.draw_scrim();
        let (cancel, affirm) = PlaintextQuestion::verbs();
        self.alert.draw(cancel, affirm);
        let frames = self.alert.frames();
        self.frames.set(Some(frames));
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

/// What [`PlaintextAlert::step`] made of an event.
pub(crate) enum AlertStep {
    /// Not the alert's: the host handles the event as it always does.
    Pass,
    /// The alert decided the event — return this.
    Done(Handled),
    /// The person answered: push the command (if any), seat focus back on the host's own
    /// controls, and return `Handled::Yes`.
    Answer(Option<SessionCmd>),
}

/// The offer a signed-in failure read-out speaks about, as its host caches it — the verdict and
/// its reason line — re-read only when the grant table moved (`plex::grant::revision`) or the
/// server the read-out is about changed.
#[derive(Clone, Debug, Default)]
pub(crate) struct OfferWatch {
    seen: Option<(u64, Option<String>)>,
    held: Option<(PlaintextVerdict, CString)>,
}

/// Which offer a read-out speaks about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Near {
    /// Only this server's: a Library source's read-out is about one server.
    Only,
    /// This server's, else the first offered: Home failed as a whole, so any server discovery
    /// offers the question for is one it could not reach.
    First,
}

impl OfferWatch {
    /// Re-read the offer for `machine` (`near` says whether another will do); `true` when what
    /// the read-out shows changed.
    pub(crate) fn refresh(&mut self, machine: Option<&str>, near: Near) -> bool {
        let key = (crate::plex::grant::revision(), machine.map(str::to_owned));
        if self.seen.as_ref() == Some(&key) {
            return false;
        }
        self.seen = Some(key);
        let offers = crate::plex::grant::offers();
        let at = offers.iter().position(|v| Some(v.machine_id.as_str()) == machine)
            .or_else(|| (near == Near::First && !offers.is_empty()).then_some(0));
        let next = at.and_then(|i| offers.into_iter().nth(i)).map(|v| {
            let line = reason(&v);
            (v, line)
        });
        let changed = next.as_ref().map(|(v, _)| v) != self.held.as_ref().map(|(v, _)| v);
        self.held = next;
        changed
    }

    /// The offer the read-out speaks about.
    pub(crate) fn verdict(&self) -> Option<&PlaintextVerdict> {
        self.held.as_ref().map(|(v, _)| v)
    }

    /// Its reason line.
    pub(crate) fn reason(&self) -> Option<&CStr> {
        self.held.as_ref().map(|(_, r)| r.as_c_str())
    }
}

/// The detail line under a Settings *Unencrypted connections* switch (the row's title is the
/// server's name): what the switch MEANS first, whichever way it stands, then — only while a grant
/// carries the server — that it is connected without encryption now.
pub(crate) fn settings_detail(on: bool, connected: bool) -> &'static str {
    match (on, connected) {
        (false, _) => "Not allowed. Only encrypted connections.",
        (true, true) => "Allowed on this network. Connected without encryption.",
        (true, false) => "Allowed on this network when a secure connection fails.",
    }
}
