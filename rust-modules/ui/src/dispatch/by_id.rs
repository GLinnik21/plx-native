//! **The by-id input** (`focus:<id>`, `press:<id>[@<subject>]`): one ingress-only input the
//! dispatcher acts on at its ORDERED delivery position, never in the ingest pre-pass.
//!
//! At that position every earlier input of the batch has already acted, so `[Right, press:X]`
//! arms on X and `[focus:X, ok]` presses X. The dispatcher (not a screen) then:
//!
//! 1. resolves the id against the input owner's view only (`owner_view`, so an id of the page under
//!    a modal is `Unknown`: the modal owns input), asking the view with the engine's focus as this
//!    position finds it (not as the drain began), through the replay hook
//!    [`ReplayAuthority::resolve_id`](super::ReplayAuthority::resolve_id);
//! 2. for a press, refuses an element that is not `loaded` (`unloaded`), then, for a press with a
//!    subject, compares the element's `subject` as the page stands AT THAT POSITION (the race between
//!    a `state` read and the press is closed there, and only up to the arm: see "The dip" below);
//! 3. on success abandons a press armed elsewhere (`abandon_if_off`, the rule a direction key
//!    uses), sets focus with `By::Script` and notes the d-pad (hover is not live);
//! 4. pushes to the FRONT of the queue, in order, the `FocusMoved{by: Script}` delivery (when focus
//!    moved) and, for a press, an ordinary OK `Down` and `Up`.
//!
//! A press on the element focus ALREADY rests on moves nothing (`FocusEngine::set` answers `Nothing`) and
//! delivers no `FocusMoved`, so a screen cannot tell it from a human's OK. The screen's own answer is
//! its projection of the FOCUSED element: while its OK would land somewhere else than on that element
//! (Home's hero/grid door, while the picture lags a Dir key's move: about 8 frames, 130 ms at 60 fps: the dive's midpoint), the screen projects it
//! `loaded: false`, and step 2 refuses ANY press of it as `unloaded`, subject or not (a linked heading
//! has none to compare). The caller asks again once the screen has settled; `focus:` asks no `loaded`.
//!
//! The OK Down is delivered next and the screen sees it exactly as a remote OK; unconsumed, it
//! arms on the engine's current key, which is now the element. The Up follows, and the delivery
//! path applies its release in order after the arm. A later input of the same batch that moves
//! focus off the element abandons the uncommitted press, so an accepted press acts on the named
//! element or on nothing, and a screen whose OK lands elsewhere says so by projecting the element
//! not loaded (above). **All or nothing**: a refusal moves no focus and emits no key.
//!
//! **The dip is the screen's.** Step 2 closes the race at the delivery, and the press it queues
//! commits a dip later (`MIN_DIP_MS` + `COMMIT_MS`, about 200 ms), when the screen re-reads what the
//! element acts on. What keeps that title from changing inside the dip is the screen reading the
//! press through its context (`cx.press`: `live` and `owner`; a delivery reads it as it stands at its
//! position, so the Tick of the frame that armed the press already sees it) and letting nothing change
//! the title while one stands on its element. Home's carousel does (`HomeScreen::hero_press_armed`):
//! while a press stands on a hero control the 8 s countdown restarts from its full length every tick,
//! as a focus move restarts it, so no turn falls due inside the press and none is owed after it (the
//! next one comes a full interval later). `HomeCmd::Flip` and Left/Right at the hero's edge are
//! REFUSED while the press stands (the page answers `Handled::No` and the turn is dropped, not
//! applied when the press settles), so the caller sends them again once it has. A hero press
//! therefore plays the title it was accepted against. A landing that takes the title OUT of the pool
//! is not held (there is no such title to play): the press acts on its successor.
//!
//! A refusal leaves one `Fx::Log` line carrying words and ids only (`a11y: refused <act> <id>
//! <reason> owner=<ns>`). The label and subject of an element never reach it.

use super::*;
use crate::a11y::A11yPath;
use plx_machine::machine::LogLine;

/// Why one by-id input was refused. The wire spelling is [`IdRefusal::code`] (`0` is "not refused").
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum IdRefusal {
    /// No such element in the owner's view (including a namespace that is not the owner's).
    Unknown,
    /// The television keyboard, or no page, owns input, or the owner changed earlier in the
    /// batch so the delivery was addressed to a screen that no longer owns it.
    NoOwner,
    /// A press on an element that is not loaded: a paged slot whose data has not landed, or the
    /// element a screen's OK would not land on yet (Home's focused element while its picture lags the
    /// hero/grid door). Not an error: ask again once the screen has settled.
    Unloaded,
    /// A press whose `@<subject>` is not the element's subject.
    SubjectMismatch,
}

impl IdRefusal {
    pub const fn code(self) -> u8 {
        match self {
            Self::Unknown => 1,
            Self::NoOwner => 2,
            Self::Unloaded => 3,
            Self::SubjectMismatch => 4,
        }
    }

    pub const fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(Self::Unknown),
            2 => Some(Self::NoOwner),
            3 => Some(Self::Unloaded),
            4 => Some(Self::SubjectMismatch),
            _ => None,
        }
    }

    /// The word the refusal line carries.
    pub const fn word(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::NoOwner => "no_owner",
            Self::Unloaded => "unloaded",
            Self::SubjectMismatch => "subject_mismatch",
        }
    }
}

/// The complete answer of one by-id resolution, before any dependent effect is emitted: either
/// the element it names (and the group it is in), or the reason it was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IdAnswer<K> {
    pub focus: Option<FocusKey<K>>,
    pub group: Option<GroupId>,
    pub refusal: Option<IdRefusal>,
}

impl<K> IdAnswer<K> {
    pub const fn refused(why: IdRefusal) -> Self {
        Self { focus: None, group: None, refusal: Some(why) }
    }
}

/// May this text ride after the `@` of `press:<id>@<subject>`? A subject is a catalog key (a
/// rating key), so it is printable ASCII with no space and no `@`. Shared by the FIFO parser and
/// the recording decoder, so the two cannot disagree.
pub fn by_id_subject_ok(subject: &str) -> bool {
    !subject.is_empty() && subject.bytes().all(|b| b.is_ascii_graphic() && b != b'@')
}

impl<H: Host> Dispatcher<H>
where
    H::Elem: IndexElem,
{
    /// Resolve one id against the OWNER's view. Pure: it reads, it never moves focus.
    fn resolve_by_id(
        &self,
        rig: &mut dyn Rig<H>,
        parts: &CxParts<H::Elem>,
        entry: EntryId,
        id: &str,
        press: bool,
        expect: Option<&str>,
    ) -> IdAnswer<H::Elem> {
        let Ok(path) = A11yPath::parse_id(id) else { return IdAnswer::refused(IdRefusal::Unknown) };
        let Some(view) = Self::owner_view(&self.nav, entry) else {
            return IdAnswer::refused(IdRefusal::NoOwner);
        };
        // The namespace is the owner's own word, or `nav` while the strip is composed. A path of
        // any other namespace names nothing in this view.
        let named = match path.ns() {
            Some("nav") => view.strip.is_some(),
            Some(ns) => ns == view.page.a11y_ns(),
            None => false,
        };
        if !named {
            return IdAnswer::refused(IdRefusal::Unknown);
        }
        let Split { views, measure, .. } = rig.split();
        let mut cx = parts.cx::<H>(views, measure);
        // `parts` was built before the drain, so its focus is the one from before any earlier key of
        // this batch (`[Down, press:X]`): the screen's projection (`a11y`'s `loaded` of its focused
        // element) is asked with the engine's focus as it stands at this delivery position.
        cx.focus = self.input.engine.read(InputOwner::Entry(entry));
        let Some(elem) = view.a11y_find(&path, &cx) else { return IdAnswer::refused(IdRefusal::Unknown) };
        let Some(a11y) = view.a11y(&elem, &cx) else { return IdAnswer::refused(IdRefusal::Unknown) };
        // An element the owner cannot place is not one a recording could replay.
        if view.place(&elem, &cx, At::SpringTarget).is_none() {
            return IdAnswer::refused(IdRefusal::Unknown);
        }
        if press {
            if !a11y.loaded {
                return IdAnswer::refused(IdRefusal::Unloaded);
            }
            if let Some(want) = expect {
                if a11y.subject.as_deref() != Some(want) {
                    return IdAnswer::refused(IdRefusal::SubjectMismatch);
                }
            }
        }
        IdAnswer { focus: Some(FocusKey { entry, elem }), group: view.group_of(&elem, &cx), refusal: None }
    }

    /// The refusal line (D3): words and ids only. An id that does not parse is never echoed.
    fn by_id_refusal_line(&self, act: &str, id: &str, why: IdRefusal) -> LogLine {
        let owner = if self.input.keyboard {
            "keyboard"
        } else {
            self.owner_entry()
                .and_then(|entry| self.nav.entry(entry))
                .and_then(|entry| entry.inst.as_ref())
                .map_or("none", |inst| inst.screen.a11y_ns())
        };
        let id = if A11yPath::parse_id(id).is_ok() { id } else { "malformed" };
        LogLine(format!("a11y: refused {act} {id} {} owner={owner}", why.word()))
    }

    /// A by-id input the ingest pre-pass could not address to any page. It still resolves (as a
    /// `NoOwner` refusal, graded and recorded like every other answer) and logs its line.
    pub(super) fn refuse_unaddressed_by_id(
        &mut self,
        rig: &mut dyn Rig<H>,
        f: u64,
        ev: &InputEvent<H::Elem>,
        head: &mut Vec<Stamped<H>>,
    ) {
        let InputKind::ById { id, press, .. } = &ev.kind else { return };
        let act = if press.is_some() { "press" } else { "focus" };
        let actual = (frame_authority(rig, |a| a.resolution_mode()) != Some(false))
            .then(|| IdAnswer::refused(IdRefusal::NoOwner));
        let Some(answer) = frame_authority(rig, |a| a.resolve_id(f, EntryId(0), actual)) else {
            frame_authority(rig, |a| a.resolution_error("missing id resolution"));
            return;
        };
        let refusal = match (answer.refusal, answer.focus, answer.group) {
            (Some(why), None, None) => why,
            _ => {
                frame_authority(rig, |a| a.resolution_error("impossible id resolution"));
                return;
            }
        };
        head.push(Stamped { from: MachineId::Input, fx: Fx::Log(self.by_id_refusal_line(act, id, refusal)) });
    }

    /// Act on one by-id input at its delivery position. `inst` is the instance the delivery was
    /// addressed to at ingest. `instance_owns_input` is the verdict of the owner guard in
    /// `execute_deliver`, which runs first: `false` means input moved away from `inst` after
    /// ingest (an earlier delivery of the batch, or a commit before a carried delivery ran), so the
    /// input resolves as a `NoOwner` refusal. The television keyboard owns input without moving the
    /// page's entry, so it is checked here.
    pub(super) fn deliver_by_id(
        &mut self,
        rig: &mut dyn Rig<H>,
        parts: &CxParts<H::Elem>,
        inst: InstanceId,
        ev: &InputEvent<H::Elem>,
        instance_owns_input: bool,
        out: &mut Vec<Stamped<H>>,
    ) {
        let InputKind::ById { id, press, expect } = &ev.kind else { return };
        let act = if press.is_some() { "press" } else { "focus" };
        let entry = self.nav.entry_of_instance(inst);
        let owned = instance_owns_input && !self.input.keyboard && entry.is_some();
        let row_entry = entry.unwrap_or(EntryId(0));
        // Targets mode feeds the recorded answer without computing one; every other mode computes
        // it, and Resolve grades it against the row.
        let actual = if frame_authority(rig, |a| a.resolution_mode()) == Some(false) {
            None
        } else if owned {
            Some(self.resolve_by_id(rig, parts, row_entry, id, press.is_some(), expect.as_deref()))
        } else {
            Some(IdAnswer::refused(IdRefusal::NoOwner))
        };
        let Some(answer) = frame_authority(rig, |a| a.resolve_id(self.frame, row_entry, actual)) else {
            frame_authority(rig, |a| a.resolution_error("missing id resolution"));
            return;
        };
        if frame_authority(rig, |a| a.resolution_active()) {
            let valid = match (answer.refusal, answer.focus) {
                (None, Some(key)) => owned && key.entry == row_entry
                    && self.valid_resolution_key(rig, parts, key, answer.group, true),
                (Some(_), None) => answer.group.is_none(),
                _ => false,
            };
            if !valid {
                frame_authority(rig, |a| a.resolution_error("impossible id resolution"));
                return;
            }
        }
        let (Some(key), None) = (answer.focus, answer.refusal) else {
            let why = answer.refusal.unwrap_or(IdRefusal::Unknown);
            out.push(Stamped { from: MachineId::Input, fx: Fx::Log(self.by_id_refusal_line(act, id, why)) });
            return;
        };
        let owner = InputOwner::Entry(key.entry);
        // The shared rule a direction key uses: a press armed on another element is abandoned,
        // never activating the element focus lands on.
        self.input.abandon_if_off(key.entry, key);
        self.input.hit.note_dpad();
        let mut front: Vec<Stamped<H>> = Vec::new();
        if let Outcome::Moved { from, to, by } =
            self.input.engine.set(owner, key, answer.group, crate::screen::By::Script)
        {
            front.push(Stamped {
                from: MachineId::Input,
                fx: Fx::Deliver(MachineId::Instance(inst), Delivery::Screen(ScreenEvent::FocusMoved { from, to, by })),
            });
        }
        if let Some(tap) = press {
            for edge in [Edge::Down, Edge::Up] {
                front.push(Stamped {
                    from: MachineId::Input,
                    fx: Fx::Deliver(
                        MachineId::Instance(inst),
                        Delivery::Screen(ScreenEvent::Input(InputEvent {
                            at: ev.at,
                            source: ev.source,
                            kind: InputKind::Key { key: Key::Ok, sym: tap.sym, wcode: tap.wcode, edge, at_edge: false },
                        })),
                    ),
                });
            }
        }
        // To the FRONT, in order: the announcement and the OK edges run before anything else this
        // frame queued, and after every input that was ahead of this one.
        for s in front.into_iter().rev() {
            self.queue.push_front(s);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refusal_has_one_code_one_word_and_round_trips() {
        for why in [IdRefusal::Unknown, IdRefusal::NoOwner, IdRefusal::Unloaded, IdRefusal::SubjectMismatch] {
            assert_eq!(IdRefusal::from_code(why.code()), Some(why));
            assert!(why.code() != 0, "zero means not refused on the wire");
            assert!(!why.word().is_empty() && why.word().bytes().all(|b| b.is_ascii_lowercase() || b == b'_'));
        }
        assert_eq!(IdRefusal::from_code(0), None);
        assert_eq!(IdRefusal::from_code(5), None);
    }

    #[test]
    fn a_subject_is_printable_ascii_without_a_space_or_an_at() {
        assert!(by_id_subject_ok("12345"));
        assert!(by_id_subject_ok("s00ff00aa"));
        for bad in ["", "a b", "a@b", "é", "a\nb"] {
            assert!(!by_id_subject_ok(bad), "{bad:?}");
        }
    }
}
