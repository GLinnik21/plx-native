//! `TabContainer` (restructure spec §6.2): the shared top strip as a `Row` group the container
//! contributes ABOVE the page's groups, over ONE shared `NavStack<PageDip>` — Home is the single
//! root; a pill is `NavOp::Root`. The page declares `strip_reachable()` (Home: false while snapped
//! to the grid) and a `Link{STRIP, Down, <entry group>}`; both are `Screen` methods with defaults.
//! A pill's activation is `ScreenEvent::Activate` on the page.
//!
//! The pills' rects come from the strip as DRAWN (`widgets::tab_pill_rects` on the legacy strip,
//! the container's own draw from phase 8); until then the application hands them in per frame.

use super::super::machine::{EntryId, GroupId, Host};
use super::super::screen::{AxisMask, EdgeRule, ElemKind, GroupKind, GroupSpec, Seat};
use super::super::Rect;
use super::stack::NavStack;
use super::transition::Transition;

/// The strip's group id — a library constant so a page's `Link` can name it.
pub const STRIP: GroupId = GroupId(0xFFFF_0001);

pub struct TabContainer<H: Host> {
    pub stack: NavStack<H>,
    /// The pills, in strip order: what each selects.
    pub pills: Vec<H::Arg>,
    /// The pills' rects as drawn this frame (screen space).
    pub pill_rects: Vec<Rect>,
    /// The selected pill: the destination's while a transition is in flight (`ui::nav::view_tab`).
    pub selected: usize,
}

impl<H: Host> TabContainer<H> {
    pub fn new(transition: Box<dyn Transition>) -> Self {
        Self {
            stack: NavStack::new(transition),
            pills: Vec::new(),
            pill_rects: Vec::new(),
            selected: 0,
        }
    }

    /// The strip as a focus group — contributed above the page's when the page allows it.
    pub fn strip_group(&self) -> GroupSpec {
        let extent = self
            .pill_rects
            .iter()
            .fold(None::<Rect>, |acc, r| Some(acc.map_or(*r, |a| a.union(*r))))
            .unwrap_or(Rect::new(0.0, 0.0, 0.0, 0.0));
        GroupSpec {
            id: STRIP,
            kind: GroupKind::Row { wrap: false },
            seat: Seat::Nearest,
            reachable: AxisMask::VERTICAL,
            edge: [EdgeRule::Geometric; 4],
            extent,
            len: self.pill_rects.len(),
            elem: ElemKind::Control,
        }
    }

    /// The entry the strip belongs to — the top page (the strip is chrome on the page).
    pub fn strip_entry(&self) -> Option<EntryId> {
        self.stack.top().map(|e| e.id)
    }

    /// Select a pill: `Root(pill)` on the shared stack; the capsule moves on the press frame.
    pub fn select(&mut self, i: usize, ret: super::super::screen::ReturnState<H::Elem, H::Memory>) {
        let Some(arg) = self.pills.get(i).cloned() else {
            return;
        };
        self.selected = i;
        self.stack
            .request(super::super::machine::NavOp::Root(arg), ret);
    }
}
