//! `TabContainer` (restructure spec §6.2): the shared top strip as a `Row` group the container
//! contributes ABOVE the page's groups, over ONE shared `NavStack<PageDip>` — Home is the single
//! root; a pill is `NavOp::SelectTab`, which covers-and-mints over the existing root rather than
//! replacing it (`NavOp::Root` is the other half of that split: a true replace, root included,
//! for a sign-in/sign-out/profile-switch reset rather than a peer press). The page declares
//! `strip_reachable()` (Home: false while snapped
//! to the grid) and a `Link{STRIP, Down, <entry group>}`; both are `Screen` methods with defaults.
//! A pill's activation is `ScreenEvent::Activate` on the page.
//!
//! The application publishes strip members by stable control identity, with drawn and target
//! geometry from the shared renderer. Removing a destination never renumbers another control.
//!
//! **A cover-and-mint's focus arrival always animates, whichever pill sent it** (issue #17,
//! fixed 2026-09-28). `NavStack::apply`'s `SelectTab` "cover-and-mint" arm — a peer pill pressed
//! while a DIFFERENT root is up, e.g. Home or Search → TV Shows — used `Self::fresh`
//! (`FocusTarget::FirstInGroup`, `By::Restore`) exactly like a `Push`/`Root` mint, which several
//! screens (`library::LibraryScreen`'s `pop_from_rest`) treat as "restored to where it was" and
//! do not animate. A tab press always arrives FROM the visible strip, so that read was never
//! right the first time a tab is visited this session, and it produced a hard snap onto the
//! first content tile with no transition. `Self::fresh_tab` (`FocusTarget::
//! FirstInGroupAnimated`, `By::Dir`) is the fix: same seat, reported as a deliberate move, so the
//! arrival animates the same as it already did for a Movies → TV Shows peer switch — which never
//! reaches `NavStack` at all (`app::bridge::nav_peer`'s early return: both are `AppArg::Library`,
//! `same_instance` by `id()` alone regardless of section — see `registry.rs`), so its own
//! directional DOWN press into the grid was always `By::Dir` already. The fix is in the shared
//! `SelectTab` mint path, not a TV Shows special case: it applies to every peer pill equally.
//! See `machine.rs`'s doc on `FocusTarget::FirstInGroupAnimated` and the regression test
//! `containers::tests::a_fresh_tab_mint_reports_a_deliberate_move_not_a_restore`.

use super::super::machine::{EntryId, GroupId, Host};
use super::super::screen::{AxisMask, EdgeRule, ElemKind, GroupKind, GroupSpec, Seat};
use super::super::Rect;
use super::stack::NavStack;
use super::transition::Transition;

/// The strip's group id — a library constant so a page's `Link` can name it.
pub const STRIP: GroupId = GroupId(0xFFFF_0001);

#[derive(Clone, Copy)]
pub struct StripMember<K> {
    pub elem: K,
    pub drawn: Rect,
    pub target: Rect,
    pub clip: Rect,
}

impl<K> StripMember<K> {
    pub fn new(elem: K, rect: Rect) -> Self {
        Self { elem, drawn: rect, target: rect, clip: Rect::FULL }
    }
}

pub struct TabContainer<H: Host> {
    pub stack: NavStack<H>,
    /// The pills, in strip order: what each selects.
    pub pills: Vec<H::Arg>,
    /// Visual order is independent of identity. Geometry is render state, keys are logical state.
    pub strip: Vec<StripMember<H::Elem>>,
    /// Preferred surviving destination if the focused member disappears. Independent of visual
    /// order: a profile chip may be first without being the application's recovery destination.
    pub strip_fallback: Option<H::Elem>,
    /// The selected pill: the destination's while a transition is in flight (`ui::nav::view_tab`).
    pub selected: usize,
}

impl<H: Host> TabContainer<H> {
    pub fn new(transition: Box<dyn Transition>) -> Self {
        Self {
            stack: NavStack::new(transition),
            pills: Vec::new(),
            strip: Vec::new(),
            strip_fallback: None,
            selected: 0,
        }
    }

    /// The strip as a focus group — contributed above the page's when the page allows it.
    pub fn strip_group(&self) -> GroupSpec {
        let extent = self
            .strip
            .iter()
            .fold(None::<Rect>, |acc, member| Some(acc.map_or(member.target, |a| a.union(member.target))))
            .unwrap_or(Rect::new(0.0, 0.0, 0.0, 0.0));
        GroupSpec {
            id: STRIP,
            kind: GroupKind::Row { wrap: false },
            seat: Seat::Nearest,
            reachable: AxisMask::VERTICAL,
            // The strip's horizontal endpoints are terminal, independent of where a page's
            // other groups happen to be placed. Only UP/DOWN may cross to page content.
            edge: [EdgeRule::Geometric, EdgeRule::Geometric, EdgeRule::Stop, EdgeRule::Stop],
            extent,
            len: self.strip.len(),
            elem: ElemKind::Control,
        }
    }

    /// The entry the strip belongs to — the top page (the strip is chrome on the page).
    pub fn strip_entry(&self) -> Option<EntryId> {
        self.stack.top().map(|e| e.id)
    }

    /// Select a pill: `SelectTab(pill)` on the shared stack; the capsule moves on the press frame.
    pub fn select(&mut self, i: usize, ret: super::super::screen::ReturnState<H::Elem, H::Memory>) {
        let Some(arg) = self.pills.get(i).cloned() else {
            return;
        };
        self.selected = i;
        self.stack
            .request(super::super::machine::NavOp::SelectTab(arg), ret);
    }
}
