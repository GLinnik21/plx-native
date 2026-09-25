//! `NavStack` (restructure spec §6.2): a stack of `Entry`s under one `Transition`. `request` captures
//! the top's `ReturnState` NOW and parks the op; the op APPLIES at the transition's commit point
//! (`Immediate`: the same commit; `PageDip`: the floor), producing the §3.4 lifecycle sequence as
//! DATA — a list of [`Life`] steps the dispatcher executes (construct through the one `Mounter`,
//! publish `Mount` and its effects in the post-commit drain, retire the bodies). A `PageDip` may
//! construct its pending destination during the OUT half so a recording painter can warm text;
//! the body remains staged, outside the live stack, and its lifecycle/effects still begin only at
//! the floor. The container decides WHAT
//! happens to WHOM and in WHICH ORDER; it never calls a screen.
//!
//! Identity: an `EntryId` is minted when the entry is created and survives body eviction; an
//! `InstanceId` is minted at mount by the dispatcher (the `Minter` is `Navigation`'s). Eviction at
//! `CAP` drops the oldest body below the top (its `Unmount` is delivered, its inflight retired);
//! the entry stays, and a Pop that reaches it remounts from its `ReturnState` — which is what
//! `an_evicted_entry_keeps_its_focus_identity_on_remount` grades.

use super::super::machine::{EntryId, GroupId, Host, InstanceId, Leave, NavOp, RequestId, Stamped};
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
    /// Constructed behind a dip but not in the live stack yet. Its mount effects and `Mount`
    /// lifecycle are released only when the navigation commits at the floor.
    pub(crate) staged: bool,
    pub(crate) staged_effects: Vec<Stamped<H>>,
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
    /// A destination which did not already exist in the stack, constructed early solely so its
    /// text can be recorded. It becomes an ordinary entry at the transition floor.
    staged: Option<Entry<H>>,
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
            .chain(self.pending.iter().filter_map(|p| p.staged.as_ref()))
            .find(|e| e.id == id)
    }

    pub fn entry_mut(&mut self, id: EntryId) -> Option<&mut Entry<H>> {
        self.entries
            .iter_mut()
            .chain(self.retired.iter_mut())
            .chain(self.pending.iter_mut().filter_map(|p| p.staged.as_mut()))
            .find(|e| e.id == id)
    }

    pub fn is_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// **Is the COMMITTED stack a settled `Root(arg)`** — one entry, that entry MOUNTED (a body,
    /// not merely a bare `Entry` waiting for the apply arm's `Mount`), `same_instance` `arg`, and
    /// nothing pending? This is the one definition `is_inert`'s `Root` arm and
    /// `app/bridge.rs::top_settled_on` both answer from, so a caller asking "has the reset to
    /// `arg` actually landed" (a per-frame follower presenting a surface OVER that landing) and
    /// the dedup deciding whether a THIRD `Root(arg)` request is redundant can never disagree
    /// about what "settled" means.
    pub fn root_settled(&self, arg: &H::Arg) -> bool {
        !self.is_pending()
            && self.entries.len() == 1
            && self.entries[0].arg.same_instance(arg)
            && self.entries[0].inst.is_some()
    }

    /// Withdraw whatever is parked, unconditionally — for a reset that is about to empty the
    /// stack out from under it. A pending op that survived a reset would apply, at its own floor,
    /// over a tree the reset already emptied (`Navigation::reset_for_profile`'s own doc: "the next
    /// `Root` rebuilds the tree" is only true if nothing older is still queued to run first).
    pub fn clear_pending(&mut self) {
        self.pending = None;
        self.due = false;
    }

    /// Queue an op (§6.2): the top's `ReturnState` is captured NOW, the transition is asked to
    /// run, and the newest request wins. `ret` is what the dispatcher read off the engine.
    ///
    /// **A redundant request is inert** — dropped before it touches `pending` or the transition —
    /// which is what lets a per-frame caller simply ask for what it wants every frame without
    /// re-kicking a `PageDip` that has nothing left to do (`Popover`/`FirstRunConsent` mounting
    /// and unmounting forever behind a repeated `Root(Profiles)` was the shape of the bug this
    /// closes: the transition never reached `Idle`, so its own floor kept re-covering and
    /// re-orphaning whatever was presented over it — TV 2026-09-17). See [`Self::is_inert`].
    pub fn request(&mut self, op: NavOp<H::Arg>, ret: ReturnState<H::Elem, H::Memory>) {
        if self.is_inert(&op) {
            return;
        }
        let from = self.top().map(|e| e.id);
        let continuous = self.continuous_for(&op);
        if let Some(top) = self.top_mut() {
            top.ret = ret;
        }
        self.pending = Some(Pending { op, from, staged: None });
        self.transition.request(continuous);
        if self.transition.commit_point() == CommitPoint::Immediate {
            self.due = true;
        }
    }

    /// **Is `op` a no-op right now?** Two independent reasons, either enough on its own:
    ///
    /// - Nothing is pending, and the COMMITTED stack already satisfies `op` — `Root(arg)` with the
    ///   stack exactly `[arg]` AND THAT ENTRY MOUNTED ([`Self::root_settled`] — a bodyless single
    ///   entry is not yet the `Mount` the apply arm would still owe it, so that request is NOT
    ///   inert), `SelectTab(arg)` with the top the root and the root `same_instance` `arg`, or
    ///   `PopTo(id)` naming the entry already on top.
    /// - Something IS pending, and it is the same kind of op with a `same_instance` argument — a
    ///   third `Root(Profiles)` while a `Root(Profiles)` is already parked asks nothing new.
    ///
    /// A *different* pending op is never inert this way: the newest request still replaces it
    /// (sign-out and a profile switch depend on newest-wins over whatever the previous frame
    /// parked). `Root`/`SelectTab`/`PopTo` are the three the redundant per-frame followers in
    /// `app/run.rs` ask for. `Push`/`Replace` are the two that MINT a destination, and a repeat of
    /// one while it is pending is a landing re-asking for the page it is already waiting on — the
    /// player pushed by `start_playback`, then again by `update`'s plan landing while the route
    /// still names the page under the dip. Replacing the pending op there would drop the body the
    /// dip-out already prepared ([`Self::stage_pending_target`]) and prepare a fresh one, and the
    /// prepared mount is where a screen spends its one-shot seed: the replacement mounts WITHOUT
    /// it (a player with no origin, which is how BACK from a film landed on Home).
    /// `Present`/`Pop`/`Dismiss`/`Cancel` are never inert here.
    fn is_inert(&self, op: &NavOp<H::Arg>) -> bool {
        if let Some(p) = &self.pending {
            return match (&p.op, op) {
                (NavOp::Root(a), NavOp::Root(b)) => a.same_instance(b),
                (NavOp::SelectTab(a), NavOp::SelectTab(b)) => a.same_instance(b),
                (NavOp::Push(a), NavOp::Push(b)) => a.same_instance(b),
                (NavOp::Replace(a), NavOp::Replace(b)) => a.same_instance(b),
                (NavOp::PopTo(a), NavOp::PopTo(b)) => a == b,
                _ => false,
            };
        }
        match op {
            NavOp::Root(arg) => self.root_settled(arg),
            NavOp::SelectTab(arg) => {
                let root = self.root().map(|e| e.id);
                self.top().map(|e| e.id) == root
                    && self.root().map_or(false, |r| {
                        r.arg.same_instance(arg) && r.inst.is_some()
                    })
            }
            NavOp::PopTo(id) => {
                self.top().map_or(false, |t| t.id == *id && t.inst.is_some())
            }
            _ => false,
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
        if self.transition.tick_presented(t, present, crate::gfx::snapshot_pending()) && self.pending.is_some() {
            self.due = true;
        }
    }

    /// Ensure a new pending destination has an entry identity before the floor. Existing pop and
    /// pop-to destinations already have one. A bodyless existing entry is deliberately skipped:
    /// remounting it ahead of `Uncover` would expose it to live deliveries before commit.
    pub fn stage_pending_target(&mut self, ids: &mut Minter) -> Option<EntryId> {
        if !self.transition.prewarms_text() {
            return None;
        }
        if let Some(id) = self.pending.as_ref().and_then(|p| p.staged.as_ref()).map(|e| e.id) {
            return Some(id);
        }
        let existing = match self.pending.as_ref().map(|p| &p.op)? {
            NavOp::Pop => self.under_top().map(|e| e.id),
            NavOp::PopTo(id) | NavOp::Dismiss(id) => Some(*id),
            NavOp::SelectTab(arg) => self.root().filter(|e| e.arg.same_instance(arg)).map(|e| e.id),
            NavOp::Push(_) | NavOp::Root(_) | NavOp::Replace(_) => None,
            NavOp::Present(_) | NavOp::Cancel => return None,
        };
        if let Some(id) = existing {
            return self.entry(id).and_then(|e| e.inst.as_ref()).map(|_| id);
        }
        let arg = match self.pending.as_ref().map(|p| &p.op)? {
            NavOp::Push(arg) | NavOp::Root(arg) | NavOp::Replace(arg) | NavOp::SelectTab(arg) => arg.clone(),
            _ => return None,
        };
        let id = ids.entry();
        let staged = Entry {
            id,
            arg,
            ret: ReturnState::default(),
            inst: None,
            evicted: false,
        };
        self.pending.as_mut().expect("read above").staged = Some(staged);
        Some(id)
    }

    /// The prepared destination screen, while the committed top remains the outgoing page.
    pub fn pending_target_mut(&mut self) -> Option<&mut Entry<H>> {
        if self.pending.as_ref()?.staged.is_some() {
            return self.pending.as_mut()?.staged.as_mut();
        }
        let id = match &self.pending.as_ref()?.op {
            NavOp::Pop => self.entries.iter().rev().nth(1).map(|e| e.id),
            NavOp::PopTo(id) | NavOp::Dismiss(id) => Some(*id),
            NavOp::SelectTab(arg) => self.entries.first()
                .filter(|e| e.arg.same_instance(arg)).map(|e| e.id),
            _ => None,
        }?;
        self.entries.iter_mut().find(|e| e.id == id)
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
        self.apply(p.op, ids, p.staged)
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
            // A pushed page has never been seen: `FirstInGroup` ignores any cursor remembered
            // for this `(EntryId, GroupId)`, which a `ContainerGroup`'s `Seat::Remembered` would
            // otherwise read back — a page that shares neither entry nor group with anything else
            // does not need the distinction, but `Push` mints a fresh `EntryId` per page here, so
            // this arm cannot itself observe the leak `FocusTarget`'s doc on `screen.rs`
            // describes; it is fixed here anyway because `Enter::Fresh` means "never seen" for
            // every caller of this constructor, not just the ones a collision can currently bite.
            focus: FocusTarget::FirstInGroup(focus_group),
        }
    }

    fn adopt_or_mint(
        &mut self,
        ids: &mut Minter,
        arg: H::Arg,
        staged: &mut Option<Entry<H>>,
    ) -> EntryId {
        if staged.as_ref().is_some_and(|e| e.arg.same_instance(&arg)) {
            let entry = staged.take().expect("checked");
            let id = entry.id;
            self.entries.push(entry);
            id
        } else {
            self.mint(ids, arg)
        }
    }

    fn apply(&mut self, op: NavOp<H::Arg>, ids: &mut Minter, mut staged: Option<Entry<H>>) -> Vec<Life<H>> {
        let mut out = Vec::new();
        match op {
            NavOp::Push(arg) => {
                let old = self.top().map(|e| e.id);
                let new = self.adopt_or_mint(ids, arg, &mut staged);
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
                    // Already the top: nothing to unwind — but a target whose body was dropped
                    // still owes the same `Mount` the general path below gives it, or the op
                    // that named it (which `is_inert` deliberately did NOT drop, precisely
                    // because the entry is bodyless) would leave the page unmounted for good.
                    if self.entry(target).map_or(false, |e| e.inst.is_none()) {
                        out.push(Life::Mount(target));
                        out.push(Life::Ev(target, ScreenEvent::Enter(Enter::Restored)));
                    }
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
            NavOp::Root(arg) => {
                // A TRUE replace: every entry, including the root, leaves for good — unless the
                // stack is already exactly `[arg]`, which is the one case with nothing to replace
                // (a `PopTo(root)` no-op, same as below). A Login/Profiles-shaped root that a
                // later `Root` merely COVERED (the old shared `Root`/`SelectTab` arm's behaviour)
                // is exactly what let a never-retired first entry sit under every later mint
                // forever; this arm is why `Root` no longer does that.
                if self.entries.len() == 1 && self.entries[0].arg.same_instance(&arg) {
                    let r = self.entries[0].id;
                    let bodyless = self.entries[0].inst.is_none();
                    if bodyless {
                        out.push(Life::Mount(r));
                    }
                    out.push(Life::Ev(r, ScreenEvent::Uncover));
                    out.push(Life::Ev(r, ScreenEvent::Enter(Enter::Restored)));
                } else {
                    let all: Vec<EntryId> = self.entries.iter().rev().map(|e| e.id).collect();
                    for id in all {
                        out.push(Life::Ev(id, ScreenEvent::WillLeave(Leave::ForGood)));
                        out.push(Life::Unmount(id));
                        self.retire(id);
                    }
                    let new = self.adopt_or_mint(ids, arg, &mut staged);
                    out.push(Life::Mount(new));
                    out.push(Life::Ev(new, ScreenEvent::Enter(Self::fresh(GroupId(0)))));
                }
            }
            NavOp::SelectTab(arg) => {
                // The strip's pill semantics: unwind to whatever the root already is (every entry
                // ABOVE it leaves for good, top-down), then either restore that root (it is
                // already `arg`) or cover-and-mint a fresh entry over it. The root itself is never
                // retired — BACK off a pressed pill still returns to it.
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
                        let new = self.adopt_or_mint(ids, arg, &mut staged);
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
                        let new = self.adopt_or_mint(ids, arg, &mut staged);
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
                let new = self.adopt_or_mint(ids, arg, &mut staged);
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
