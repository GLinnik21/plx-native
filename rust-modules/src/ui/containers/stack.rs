//! `NavStack` (restructure spec §6.2): a stack of `Entry`s under one `Transition`. `request` captures
//! the top's `ReturnState` NOW and parks the op; the op APPLIES at the transition's commit point
//! (`Immediate`: the same commit; `PageDip`: the floor), producing the §3.4 lifecycle sequence as
//! DATA — a list of [`Life`] steps the dispatcher executes (mount through the one `Mounter`,
//! deliver the events in the post-commit drain, retire the bodies). The container decides WHAT
//! happens to WHOM and in WHICH ORDER; it never calls a screen.
//!
//! Identity: an `EntryId` is minted when the entry is created and survives body eviction; an
//! `InstanceId` is minted at mount by the dispatcher (the `Minter` is `Navigation`'s). Eviction at
//! `CAP` drops the oldest body below the top (its `Unmount` is delivered, its inflight retired);
//! the entry stays, and a Pop that reaches it remounts from its `ReturnState` — which is what
//! `an_evicted_entry_keeps_its_focus_identity_on_remount` grades.

use super::super::machine::{EntryId, GroupId, Host, InstanceId, Leave, NavOp, RequestId};
use super::super::screen::{Enter, FocusTarget, ReturnState, Screen, ScreenArg, ScreenEvent};
use super::transition::{CommitPoint, Transition};
use super::{Life, Minter};

/// Entries a stack keeps BODIES for (§6.1).
pub const CAP: usize = 16;

/// A mounted body (§5.1).
pub struct Instance<H: Host> {
    pub id: InstanceId,
    pub screen: Box<dyn Screen<H>>,
    pub inflight: Vec<RequestId>,
}

/// An entry (§6.2): identity, argument, return state, and the body while it has one.
pub struct Entry<H: Host> {
    pub id: EntryId,
    pub arg: H::Arg,
    pub ret: ReturnState<H::Elem, H::Memory>,
    pub inst: Option<Instance<H>>,
    /// The body was evicted at `CAP` (as opposed to never mounted): it remounts on `Enter(Restored)`.
    pub evicted: bool,
}

struct Pending<H: Host> {
    op: NavOp<H::Arg>,
    /// The top at request time — `cancel(from)` withdraws only if the top has not moved.
    from: Option<EntryId>,
}

pub struct NavStack<H: Host> {
    pub entries: Vec<Entry<H>>,
    /// Entries removed from the stack whose bodies still owe an `Unmount` delivery; dropped by
    /// `prune` once it was delivered.
    pub retired: Vec<Entry<H>>,
    pending: Option<Pending<H>>,
    pub transition: Box<dyn Transition>,
    /// The floor was crossed this frame (a `Floor` transition): apply at the next commit.
    due: bool,
}

impl<H: Host> NavStack<H> {
    pub fn new(transition: Box<dyn Transition>) -> Self {
        Self {
            entries: Vec::new(),
            retired: Vec::new(),
            pending: None,
            transition,
            due: false,
        }
    }

    pub fn top(&self) -> Option<&Entry<H>> {
        self.entries.last()
    }

    pub fn top_mut(&mut self) -> Option<&mut Entry<H>> {
        self.entries.last_mut()
    }

    pub fn root(&self) -> Option<&Entry<H>> {
        self.entries.first()
    }

    pub fn depth(&self) -> usize {
        self.entries.len()
    }

    pub fn entry(&self, id: EntryId) -> Option<&Entry<H>> {
        self.entries
            .iter()
            .chain(self.retired.iter())
            .find(|e| e.id == id)
    }

    pub fn entry_mut(&mut self, id: EntryId) -> Option<&mut Entry<H>> {
        self.entries
            .iter_mut()
            .chain(self.retired.iter_mut())
            .find(|e| e.id == id)
    }

    pub fn is_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// Queue an op (§6.2): the top's `ReturnState` is captured NOW, the transition is asked to
    /// run, and the newest request wins. `ret` is what the dispatcher read off the engine.
    pub fn request(&mut self, op: NavOp<H::Arg>, ret: ReturnState<H::Elem, H::Memory>) {
        let from = self.top().map(|e| e.id);
        let continuous = self.continuous_for(&op);
        if let Some(top) = self.top_mut() {
            top.ret = ret;
        }
        self.pending = Some(Pending { op, from });
        self.transition.request(continuous);
        if self.transition.commit_point() == CommitPoint::Immediate {
            self.due = true;
        }
    }

    /// Both sides wear the shared chrome (`ui::nav`'s `continuous`): the top and the destination.
    fn continuous_for(&self, op: &NavOp<H::Arg>) -> bool {
        use super::super::machine::Chrome;
        let top = self.top().map(|e| e.arg.chrome());
        let dest = match op {
            NavOp::Push(a) | NavOp::Root(a) | NavOp::SelectTab(a) | NavOp::Replace(a) | NavOp::Present(a) => {
                Some(a.chrome())
            }
            NavOp::Pop => self
                .entries
                .iter()
                .rev()
                .nth(1)
                .map(|e| e.arg.chrome()),
            NavOp::PopTo(id) | NavOp::Dismiss(id) => self.entry(*id).map(|e| e.arg.chrome()),
            NavOp::Cancel => None,
        };
        matches!((top, dest), (Some(Chrome::TabBar), Some(Chrome::TabBar)))
    }

    /// Withdraw the pending op iff the entry that asked is still the top and the transition has
    /// not committed (§6.2 `cancel(from) iff pending.from == top().id`).
    pub fn cancel(&mut self, from: EntryId) -> bool {
        let same = self.pending.as_ref().map(|p| p.from) == Some(Some(from));
        if same && self.transition.cancel() {
            self.pending = None;
            self.due = false;
            true
        } else {
            false
        }
    }

    /// One frame of the transition (§3.3 step 4, containers before pages). A floor crossed
    /// marks the pending op due for THIS frame's commit.
    pub fn tick(&mut self, t: super::super::machine::Tick, present: &mut super::super::machine::PresentHandle<'_>) {
        if self.transition.tick(t, present) && self.pending.is_some() {
            self.due = true;
        }
    }

    /// At NAV COMMIT: apply the pending op if it is due, producing the lifecycle steps.
    pub fn commit(&mut self, ids: &mut Minter) -> Vec<Life<H>> {
        if !self.due {
            return Vec::new();
        }
        self.due = false;
        let Some(p) = self.pending.take() else {
            return Vec::new();
        };
        self.apply(p.op, ids)
    }

    fn mint(&mut self, ids: &mut Minter, arg: H::Arg) -> EntryId {
        let id = ids.entry();
        self.entries.push(Entry {
            id,
            arg,
            ret: ReturnState::default(),
            inst: None,
            evicted: false,
        });
        id
    }

    /// Move an entry out of the stack into `retired`, where its body can still receive `Unmount`.
    fn retire(&mut self, id: EntryId) {
        if let Some(i) = self.entries.iter().position(|e| e.id == id) {
            let e = self.entries.remove(i);
            self.retired.push(e);
        }
    }

    /// Bodies beyond `CAP`, oldest first, below the top: evicted (§6.1).
    fn evict(&mut self, out: &mut Vec<Life<H>>) {
        let live = self.entries.iter().filter(|e| e.inst.is_some()).count();
        let mut over = live.saturating_sub(CAP.saturating_sub(1)); // the new top will mount too
        let n = self.entries.len();
        for e in self.entries.iter_mut().take(n.saturating_sub(1)) {
            if over == 0 {
                break;
            }
            if e.inst.is_some() {
                e.evicted = true;
                out.push(Life::Evict(e.id));
                over -= 1;
            }
        }
    }

    fn fresh(focus_group: GroupId) -> Enter<H::Elem> {
        Enter::Fresh {
            focus: FocusTarget::ContainerGroup(focus_group),
        }
    }

    fn apply(&mut self, op: NavOp<H::Arg>, ids: &mut Minter) -> Vec<Life<H>> {
        let mut out = Vec::new();
        match op {
            NavOp::Push(arg) => {
                let old = self.top().map(|e| e.id);
                let new = self.mint(ids, arg);
                self.evict(&mut out);
                if let Some(o) = old {
                    out.push(Life::Ev(o, ScreenEvent::WillLeave(Leave::Deeper)));
                }
                out.push(Life::Mount(new));
                out.push(Life::Ev(new, ScreenEvent::Enter(Self::fresh(GroupId(0)))));
                if let Some(o) = old {
                    out.push(Life::Ev(o, ScreenEvent::Cover));
                }
            }
            NavOp::Pop => {
                let Some(top) = self.top().map(|e| e.id) else {
                    return out;
                };
                let under = self.entries.iter().rev().nth(1).map(|e| (e.id, e.inst.is_none()));
                out.push(Life::Ev(top, ScreenEvent::WillLeave(Leave::ForGood)));
                out.push(Life::Unmount(top));
                self.retire(top);
                if let Some((u, bodyless)) = under {
                    if bodyless {
                        out.push(Life::Mount(u));
                    }
                    out.push(Life::Ev(u, ScreenEvent::Uncover));
                    out.push(Life::Ev(u, ScreenEvent::Enter(Enter::Restored)));
                }
            }
            NavOp::PopTo(target) | NavOp::Dismiss(target) => {
                if !self.entries.iter().any(|e| e.id == target) {
                    return out; // not on this stack: nothing to do
                }
                let above: Vec<EntryId> = self
                    .entries
                    .iter()
                    .rev()
                    .take_while(|e| e.id != target)
                    .map(|e| e.id)
                    .collect();
                if above.is_empty() {
                    return out;
                }
                for id in above {
                    out.push(Life::Ev(id, ScreenEvent::WillLeave(Leave::ForGood)));
                    out.push(Life::Unmount(id));
                    self.retire(id);
                }
                let bodyless = self.entry(target).map_or(false, |e| e.inst.is_none());
                if bodyless {
                    out.push(Life::Mount(target));
                }
                out.push(Life::Ev(target, ScreenEvent::Uncover));
                out.push(Life::Ev(target, ScreenEvent::Enter(Enter::Restored)));
            }
            NavOp::Root(arg) | NavOp::SelectTab(arg) => {
                // every entry above the root leaves for good, top-down
                let root = self.root().map(|e| e.id);
                let above: Vec<EntryId> = self
                    .entries
                    .iter()
                    .rev()
                    .filter(|e| Some(e.id) != root)
                    .map(|e| e.id)
                    .collect();
                for id in above {
                    out.push(Life::Ev(id, ScreenEvent::WillLeave(Leave::ForGood)));
                    out.push(Life::Unmount(id));
                    self.retire(id);
                }
                match root {
                    None => {
                        // the first root: the stack was empty
                        let new = self.mint(ids, arg);
                        out.push(Life::Mount(new));
                        out.push(Life::Ev(new, ScreenEvent::Enter(Self::fresh(GroupId(0)))));
                    }
                    Some(r) if self.entry(r).map_or(false, |e| e.arg.same_instance(&arg)) => {
                        // the root's own pill: a PopTo(root)
                        let bodyless = self.entry(r).map_or(false, |e| e.inst.is_none());
                        if bodyless {
                            out.push(Life::Mount(r));
                        }
                        out.push(Life::Ev(r, ScreenEvent::Uncover));
                        out.push(Life::Ev(r, ScreenEvent::Enter(Enter::Restored)));
                    }
                    Some(r) => {
                        out.push(Life::Ev(r, ScreenEvent::WillLeave(Leave::Deeper)));
                        let new = self.mint(ids, arg);
                        out.push(Life::Mount(new));
                        out.push(Life::Ev(new, ScreenEvent::Enter(Self::fresh(GroupId(0)))));
                        out.push(Life::Ev(r, ScreenEvent::Cover));
                    }
                }
            }
            NavOp::Replace(arg) => {
                let old = self.top().map(|e| e.id);
                if let Some(o) = old {
                    out.push(Life::Ev(o, ScreenEvent::WillLeave(Leave::ForGood)));
                    out.push(Life::Unmount(o));
                    self.retire(o);
                }
                let new = self.mint(ids, arg);
                out.push(Life::Mount(new));
                out.push(Life::Ev(new, ScreenEvent::Enter(Self::fresh(GroupId(0)))));
            }
            NavOp::Present(_) => {
                // a modal surface is the `ModalStack`'s to present; on a page stack it is a Push
                // with no chrome argument — never reached, `Navigation` routes it
            }
            NavOp::Cancel => {}
        }
        out
    }

    /// Drop retired entries whose `Unmount` was delivered, and the bodies of evicted ones.
    pub fn prune(&mut self, unmounted: &[InstanceId]) {
        self.retired
            .retain(|e| !e.inst.as_ref().map_or(true, |i| unmounted.contains(&i.id)));
        for e in &mut self.entries {
            if let Some(i) = &e.inst {
                if e.evicted && unmounted.contains(&i.id) {
                    e.inst = None;
                }
            }
        }
    }

    pub fn page_alpha(&self) -> f32 {
        self.transition.page_alpha()
    }

    pub fn chrome_alpha(&self) -> f32 {
        self.transition.chrome_alpha()
    }

    /// **The DESTINATION of a pending op, if it names one.** The pending selection the shared tab
    /// strip reads while a transition is in flight (`ui::nav::view_tab`): the capsule travels to
    /// the pressed pill on the PRESS frame, and the page it names is still fading in. WHICH PILL
    /// that argument is remains the application's answer.
    pub fn pending_dest(&self) -> Option<&H::Arg> {
        match self.pending.as_ref().map(|p| &p.op)? {
            NavOp::Push(a) | NavOp::Root(a) | NavOp::SelectTab(a) | NavOp::Replace(a) => Some(a),
            NavOp::PopTo(id) | NavOp::Dismiss(id) => self.entry(*id).map(|e| &e.arg),
            NavOp::Pop => self.under_top().map(|e| &e.arg),
            NavOp::Present(_) | NavOp::Cancel => None,
        }
    }

    /// The entry a `NavOp::Pop` would reveal — what a BACK's chrome question is asked about.
    pub fn under_top(&self) -> Option<&Entry<H>> {
        self.entries.iter().rev().nth(1)
    }
}
