//! The container tree (restructure spec §2.2, §6.2): `Navigation` = `TabContainer` → one shared
//! `NavStack` → the shared `ModalStack`. It owns every `Entry` and `Instance`, mints every
//! `EntryId` and `InstanceId`, is the sole owner of the live/inflight index behind
//! `is_deliverable`, and resolves `input_owner()` — the shared modal stack first, then the top
//! page. Structural ops arrive as `NavOp`s at NAV COMMIT and come back out as [`Life`] steps the
//! dispatcher executes; the container never calls a screen.
//!
//! Under `ui/containers/` rather than the spec's `ui/nav/` because `ui/nav.rs` is the LEGACY page
//! transition and lives until phase 12; the directory takes that name when the file goes.
#![allow(dead_code)] // phase 3b: the product mounts one LegacyPage; screens arrive from 5b

pub mod modal;
pub mod stack;
pub mod tabs;
pub mod transition;
#[cfg(test)]
mod tests;

use super::machine::{Addr, Canon, EntryId, Host, InputOwner, InstanceId, LogicalState, MachineId, NavOp, PresentHandle, Tick};
use super::geom::IndexElem;
use super::screen::{ReturnState, ScreenEvent};
use modal::{ModalStack, Style};
use stack::{Entry, Instance};
use tabs::TabContainer;
use transition::Transition;

/// One lifecycle step a container asks the dispatcher to execute (§3.4), in order.
pub enum Life<H: Host> {
    /// Mint an `InstanceId`, call the one `Mounter`, deliver `Mount`.
    Mount(EntryId),
    /// Deliver an event to the entry's body (if it has one).
    Ev(EntryId, ScreenEvent<H>),
    /// Retire the body: deliver `Unmount` last, retire its inflight.
    Unmount(EntryId),
    /// Evict the body at `CAP`: `Unmount` delivered, the entry and its `ReturnState` kept.
    Evict(EntryId),
}

/// The identity minter (§5.1): one for the whole tree.
#[derive(Default)]
pub struct Minter {
    next_entry: u32,
    next_inst: u32,
}

impl Minter {
    pub fn entry(&mut self) -> EntryId {
        self.next_entry += 1;
        EntryId(self.next_entry)
    }
    pub fn instance(&mut self) -> InstanceId {
        self.next_inst += 1;
        InstanceId(self.next_inst)
    }
}

/// What BACK does, resolved over the input owner's own stack (§3.4 `NavOpKind::Back`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BackAnswer {
    /// A pop was requested on the owner's stack.
    Popped,
    /// The owner is a modal at depth 0: it was dismissed.
    Dismissed,
    /// The owner is the root of the root stack: the application decides (the platform's Home).
    AtRoot,
}

pub struct Navigation<H: Host> {
    pub ids: Minter,
    pub tabs: TabContainer<H>,
    pub modals: ModalStack<H>,
    /// Covered page-owned surfaces remain mounted but outside the active render/input scope.
    pub covered_modals: Vec<(EntryId, ModalStack<H>)>,
    /// The tree is backgrounded (0x103/0x104): every body heard `Suspend`.
    pub suspended: bool,
    /// `Present(arg)` needs a style the library cannot read off an `Arg`; the application sets
    /// the one the next `Present` uses (the registry's job in the screen phases).
    pub next_style: Style,
}

/// Shape of the fields encoded by Navigation::write, independent of application screen types.
pub const STATE_SHAPE: &str = "Navigation{strip_fallback:Option<ElemIndex>,Entry:(EntryId,evicted:bool,H::Arg,ReturnState),strip:[Option<ElemIndex>],pages:[Entry,Option<(InstanceId,StateHash)>],surfaces:[Entry,Phase,Option<(InstanceId,StateHash)>],suspended:bool,covered:[EntryId,[Entry,Phase,Option<(InstanceId,StateHash)>]]}";

impl<H: Host> Navigation<H> {
    pub fn new(transition: Box<dyn Transition>) -> Self {
        Self {
            ids: Minter::default(),
            tabs: TabContainer::new(transition),
            modals: ModalStack::new(),
            covered_modals: Vec::new(),
            suspended: false,
            next_style: Style::Compact,
        }
    }

    // ---- the index ---------------------------------------------------------------------------

    pub fn top_page(&self) -> Option<&Entry<H>> {
        self.tabs.stack.top()
    }

    pub fn entry(&self, id: EntryId) -> Option<&Entry<H>> {
        self.tabs.stack.entry(id).or_else(|| self.modals.entry(id))
            .or_else(|| self.covered_modals.iter().find_map(|(_, m)| m.entry(id)))
    }

    pub fn entry_mut(&mut self, id: EntryId) -> Option<&mut Entry<H>> {
        if self.tabs.stack.entry(id).is_some() {
            self.tabs.stack.entry_mut(id)
        } else {
            self.modals.entry_mut(id).or_else(|| self.covered_modals.iter_mut().find_map(|(_, m)| m.entry_mut(id)))
        }
    }

    pub fn instance(&self, id: InstanceId) -> Option<&Instance<H>> {
        self.tabs
            .stack
            .entries
            .iter()
            .chain(self.tabs.stack.retired.iter())
            .chain(self.modals.surfaces.iter().map(|s| &s.entry))
            .chain(self.modals.retired.iter())
            .chain(self.covered_modals.iter().flat_map(|(_, m)| m.surfaces.iter().map(|s| &s.entry).chain(m.retired.iter())))
            .filter_map(|e| e.inst.as_ref())
            .find(|i| i.id == id)
    }

    /// The entry a body belongs to.
    pub fn entry_of_instance(&self, id: InstanceId) -> Option<EntryId> {
        self.tabs
            .stack
            .entries
            .iter()
            .chain(self.tabs.stack.retired.iter())
            .chain(self.modals.surfaces.iter().map(|s| &s.entry))
            .chain(self.modals.retired.iter())
            .chain(self.covered_modals.iter().flat_map(|(_, m)| m.surfaces.iter().map(|s| &s.entry).chain(m.retired.iter())))
            .find(|e| e.inst.as_ref().map_or(false, |i| i.id == id))
            .map(|e| e.id)
    }

    pub fn instance_mut(&mut self, id: InstanceId) -> Option<&mut Instance<H>> {
        let found = self
            .tabs
            .stack
            .entries
            .iter_mut()
            .chain(self.tabs.stack.retired.iter_mut())
            .filter_map(|e| e.inst.as_mut())
            .find(|i| i.id == id);
        if found.is_some() {
            return found;
        }
        self.modals.instance_mut(id).or_else(|| self.covered_modals.iter_mut().find_map(|(_, m)| m.instance_mut(id)))
    }

    /// Every entry with a body, bottom-to-top: the page stack, then the surfaces.
    pub fn bodies(&self) -> impl Iterator<Item = &Entry<H>> {
        self.tabs
            .stack
            .entries
            .iter()
            .chain(self.modals.surfaces.iter().map(|s| &s.entry))
            .chain(self.covered_modals.iter().flat_map(|(_, m)| m.surfaces.iter().map(|s| &s.entry)))
            .filter(|e| e.inst.is_some())
    }

    /// The live index (§5.2): an address is deliverable iff its instance is live and the
    /// request is in its `inflight`; non-instance machines are always live.
    pub fn is_deliverable(&self, addr: &Addr) -> bool {
        match addr.to {
            MachineId::Instance(id) => self
                .bodies()
                .filter_map(|e| e.inst.as_ref())
                .any(|i| i.id == id && i.inflight.contains(&addr.req)),
            _ => true,
        }
    }

    pub fn instance_of(&self, entry: EntryId) -> Option<InstanceId> {
        self.entry(entry).and_then(|e| e.inst.as_ref()).map(|i| i.id)
    }

    /// The input owner (§2.2): the shared modal stack first, then the top page.
    pub fn input_owner(&self) -> Option<InputOwner> {
        self.modals
            .input_owner()
            .or_else(|| self.top_page().map(|e| InputOwner::Entry(e.id)))
    }

    /// Is this entry a modal surface (as opposed to a page)?
    pub fn is_surface(&self, id: EntryId) -> bool {
        self.modals.surface(id).is_some()
    }

    /// A surface awaiting its evicted owner's data still precedes that page in the Back path.
    pub fn pending_surface(&self) -> Option<EntryId> {
        let host = self.top_page()?.id;
        self.covered_modals.iter().find(|(e, _)| *e == host)?.1.surfaces.iter().rev()
            .find(|s| matches!(s.phase, modal::Phase::Opening | modal::Phase::Open))
            .map(|s| s.entry.id)
    }

    // ---- structural ops ----------------------------------------------------------------------

    /// A parked `NavOp` at NAV COMMIT: page-stack ops are requested on the shared stack (and
    /// apply at the transition's commit point); `Present`/`Dismiss`/`Cancel` are immediate.
    /// Returns the immediate lifecycle steps (a surface's), if any.
    pub fn request(&mut self, op: NavOp<H::Arg>, ret: ReturnState<H::Elem, H::Memory>) -> Vec<Life<H>> {
        match op {
            NavOp::Present(arg) => {
                let host = self.top_page().map(|e| e.id);
                if let Some(InputOwner::Entry(owner)) = self.input_owner() {
                    if let Some(e) = self.entry_mut(owner) { e.ret = ret; }
                }
                let (_, mut out) = self.modals.present(&mut self.ids, arg, self.next_style);
                if let Some(h) = host {
                    if self.modals.surfaces.len() == 1 {
                        out.push(Life::Ev(h, ScreenEvent::Cover));
                    }
                }
                out
            }
            NavOp::Dismiss(id) if self.is_surface(id) => {
                let mut out = Vec::new();
                if self.modals.dismiss(id) {
                    let others_up = self
                        .modals
                        .surfaces
                        .iter()
                        .any(|s| s.entry.id != id && s.phase != modal::Phase::Closing);
                    if !others_up {
                        if let Some(h) = self.top_page().map(|e| e.id) {
                            out.push(Life::Ev(h, ScreenEvent::Uncover));
                            out.push(Life::Ev(h, ScreenEvent::Enter(crate::ui::screen::Enter::Restored)));
                        }
                    }
                }
                out
            }
            NavOp::Dismiss(id) if self.covered_modals.iter().any(|(_, m)| m.surface(id).is_some()) => {
                let modal = &mut self.covered_modals.iter_mut().find(|(_, m)| m.surface(id).is_some()).unwrap().1;
                let at = modal.surfaces.iter().position(|s| s.entry.id == id).unwrap();
                let surface = modal.surfaces.remove(at);
                modal.retired.push(surface.entry);
                vec![
                    Life::Ev(id, ScreenEvent::WillLeave(super::machine::Leave::ForGood)),
                    Life::Unmount(id),
                ]
            }
            NavOp::Cancel => {
                if let Some(top) = self.tabs.stack.top().map(|e| e.id) {
                    self.tabs.stack.cancel(top);
                }
                Vec::new()
            }
            other => {
                let ret = if let Some(InputOwner::Entry(owner)) = self.modals.input_owner() {
                    if let Some(e) = self.modals.entry_mut(owner) { e.ret = ret; }
                    self.top_page().map(|e| e.ret.clone()).unwrap_or_default()
                } else { ret };
                self.tabs.stack.request(other, ret);
                Vec::new()
            }
        }
    }

    /// BACK from the input owner (§3.4 `NavOpKind::Back`), resolved over ITS stack.
    pub fn back(&mut self, ret: ReturnState<H::Elem, H::Memory>) -> (BackAnswer, Vec<Life<H>>) {
        if let Some(id) = self.pending_surface() {
            return (BackAnswer::Dismissed, self.request(NavOp::Dismiss(id), ret));
        }
        match self.input_owner() {
            Some(InputOwner::Entry(id)) if self.is_surface(id) => {
                let out = self.request(NavOp::Dismiss(id), ret);
                (BackAnswer::Dismissed, out)
            }
            _ => {
                if self.tabs.stack.depth() <= 1 {
                    (BackAnswer::AtRoot, Vec::new())
                } else {
                    self.tabs.stack.request(NavOp::Pop, ret);
                    (BackAnswer::Popped, Vec::new())
                }
            }
        }
    }

    /// Step 4 (containers before pages): the transition and every surface's motion.
    pub fn tick(&mut self, t: Tick, present: &mut PresentHandle<'_>) {
        self.tabs.stack.tick(t, present);
        self.modals.tick(t, present);
    }

    /// NAV COMMIT: the shared stack's due op, then the surfaces whose fade finished.
    pub fn commit(&mut self) -> Vec<Life<H>> {
        let previous = self.top_page().map(|e| e.id);
        let mut out = self.tabs.stack.commit(&mut self.ids);
        let evicted: Vec<_> = out.iter().filter_map(|life| match life {
            Life::Evict(id) => Some(*id), _ => None,
        }).collect();
        let current = self.top_page().map(|e| e.id);
        if previous != current {
            if let Some(host) = previous {
                if !self.modals.surfaces.is_empty() || !self.modals.retired.is_empty() {
                    let modal = std::mem::take(&mut self.modals);
                    out.extend(modal.surfaces.iter().map(|s| Life::Ev(s.entry.id, ScreenEvent::Cover)));
                    self.covered_modals.push((host, modal));
                }
            }
            // A removed page cannot leave an orphaned modal in the live index.
            for (host, modal) in &mut self.covered_modals {
                if !self.tabs.stack.entries.iter().any(|e| e.id == *host) {
                    for surface in modal.surfaces.drain(..).rev() {
                        out.push(Life::Ev(surface.entry.id, ScreenEvent::WillLeave(super::machine::Leave::ForGood)));
                        out.push(Life::Unmount(surface.entry.id));
                        modal.retired.push(surface.entry);
                    }
                }
            }
        }
        for (host, modal) in &mut self.covered_modals {
            if evicted.contains(host) {
                for surface in &mut modal.surfaces {
                    if surface.entry.inst.is_some() {
                        surface.entry.evicted = true;
                        surface.ground_ready = false;
                        out.push(Life::Evict(surface.entry.id));
                    }
                }
            }
        }
        if let Some(i) = self.covered_modals.iter().position(|(host, _)| Some(*host) == current) {
            let needs_body = self.covered_modals[i].1.surfaces.iter().any(|s| s.entry.inst.is_none());
            let ready = self.top_page().and_then(|e| e.inst.as_ref())
                .is_some_and(|i| i.screen.covered_surfaces_ready());
            if !needs_body || ready {
                self.modals = self.covered_modals.remove(i).1;
                if let Some(host) = current { out.push(Life::Ev(host, ScreenEvent::Cover)); }
                for surface in &self.modals.surfaces {
                    if surface.entry.inst.is_none() { out.push(Life::Mount(surface.entry.id)); }
                    out.push(Life::Ev(surface.entry.id, ScreenEvent::Uncover));
                    out.push(Life::Ev(surface.entry.id, ScreenEvent::Enter(crate::ui::screen::Enter::Restored)));
                }
            }
        }
        out.extend(self.modals.prune());
        out
    }

    /// The frame tail: drop bodies whose `Unmount` was delivered.
    pub fn prune(&mut self, unmounted: &[InstanceId]) {
        self.tabs.stack.prune(unmounted);
        self.modals.drop_unmounted(unmounted);
        for (_, modal) in &mut self.covered_modals { modal.drop_unmounted(unmounted); }
        self.covered_modals.retain(|(_, m)| !m.surfaces.is_empty() || !m.retired.is_empty());
    }

    // ---- lifecycle ---------------------------------------------------------------------------

    /// 0x103/0x104: every body hears `Suspend`; the tree parks.
    pub fn suspend(&mut self) -> Vec<Life<H>> {
        self.suspended = true;
        self.bodies()
            .map(|e| Life::Ev(e.id, ScreenEvent::Suspend))
            .collect()
    }

    /// 0x105/0x106.
    pub fn resume(&mut self) -> Vec<Life<H>> {
        self.suspended = false;
        self.bodies()
            .map(|e| Life::Ev(e.id, ScreenEvent::Resume))
            .collect()
    }

    /// `NavEvent::ResetForProfile` (§6.1): every entry is dropped — surfaces first, then the
    /// page stack top-down. The next `Root` rebuilds the tree.
    pub fn reset_for_profile(&mut self) -> Vec<Life<H>> {
        let mut out = Vec::new();
        for (_, modal) in &mut self.covered_modals {
            for surface in modal.surfaces.drain(..).rev() {
                out.push(Life::Ev(surface.entry.id, ScreenEvent::WillLeave(super::machine::Leave::ForGood)));
                out.push(Life::Unmount(surface.entry.id));
                modal.retired.push(surface.entry);
            }
        }
        let surfaces: Vec<EntryId> = self.modals.surfaces.iter().rev().map(|s| s.entry.id).collect();
        for id in surfaces {
            out.push(Life::Ev(id, ScreenEvent::WillLeave(super::machine::Leave::ForGood)));
            out.push(Life::Unmount(id));
            if let Some(i) = self.modals.surfaces.iter().position(|s| s.entry.id == id) {
                let s = self.modals.surfaces.remove(i);
                self.modals.retired.push(s.entry);
            }
        }
        let pages: Vec<EntryId> = self.tabs.stack.entries.iter().rev().map(|e| e.id).collect();
        for id in pages {
            out.push(Life::Ev(id, ScreenEvent::WillLeave(super::machine::Leave::ForGood)));
            out.push(Life::Unmount(id));
            if let Some(i) = self.tabs.stack.entries.iter().position(|e| e.id == id) {
                let e = self.tabs.stack.entries.remove(i);
                self.tabs.stack.retired.push(e);
            }
        }
        out
    }

    // ---- the state hash ----------------------------------------------------------------------

    /// The tree's contribution to the logical-state hash (§5.4): entry ids, bodies' hashes,
    /// surface phases, in a fixed order.
    pub fn write(&self, c: &mut Canon) where H::Elem: IndexElem {
        c.option(self.tabs.strip_fallback.and_then(|key| key.index()), |c, key| { c.u32(key); });
        c.seq(self.tabs.strip.len());
        for member in &self.tabs.strip {
            c.option(member.elem.index(), |c, id| { c.u32(id); });
        }
        c.seq(self.tabs.stack.entries.len());
        for e in &self.tabs.stack.entries {
            write_entry(e, c);
            c.option(e.inst.as_ref(), |c, i| {
                c.u32(i.id.0);
                c.u64(i.screen.state().hash());
            });
        }
        c.seq(self.modals.surfaces.len());
        for s in &self.modals.surfaces {
            write_entry(&s.entry, c);
            c.discriminant(s.phase as u32);
            c.option(s.entry.inst.as_ref(), |c, i| {
                c.u32(i.id.0);
                c.u64(i.screen.state().hash());
            });
        }
        c.bool(self.suspended);
        c.seq(self.covered_modals.len());
        for (host, modal) in &self.covered_modals {
            c.u32(host.0).seq(modal.surfaces.len());
            for s in &modal.surfaces {
                write_entry(&s.entry, c);
                c.discriminant(s.phase as u32);
                c.option(s.entry.inst.as_ref(), |c, i| {
                    c.u32(i.id.0);
                    c.u64(i.screen.state().hash());
                });
            }
        }
    }
}

fn write_entry<H: Host>(entry: &Entry<H>, c: &mut Canon) where H::Elem: IndexElem {
    c.u32(entry.id.0).bool(entry.evicted);
    entry.arg.write(c);
    c.option(entry.ret.focus, |c, key| {
        c.u32(key.entry.0).option(key.elem.index(), |c, elem| { c.u32(elem); });
    }).f32(entry.ret.scroll).seq(entry.ret.remembered.len());
    for (group, elem) in &entry.ret.remembered {
        c.u32(group.0).option(elem.index(), |c, elem| { c.u32(elem); });
    }
    entry.ret.memory.write(c);
}
