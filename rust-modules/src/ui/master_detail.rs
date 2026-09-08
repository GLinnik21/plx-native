//! A reusable master/detail focus and render composition (restructure spec v4 §10).
//!
//! `MasterDetail` owns two [`Part`]s and the policy at the door between them. It does not own
//! focus: there is deliberately no `FocusKey`, selected row/column, or remembered cursor in its
//! state. The [`crate::ui::focus::FocusEngine`] remains the only owner of those values.
//!
//! The master group's entry policy is [`Seat::ProjectedFrom`] the designated detail group.
//! The engine supplies that group's remembered element's current reconciled placement to the
//! master part's `seat`: its `from.index` is the data index, even when entering from a toolbar.
//! The part supplies the projection policy (for example grid item index → letter, using counts
//! in `Cx::views`). No screen/view needs to publish a copy of the engine's remembered cursor.
//!
//! The caller must supply [`KeyRegion`], backed by its typed identity domain or a published
//! tombstone registry. Region ownership must survive item deletion and must not be inferred from
//! current visible membership. Reconciliation asks that owning child first and accepts only a
//! fallback that the child can place. The classifier is an identity query, never a focus cache.
//!
//! A screen owner feeds delivered `FocusMoved` and activation/commit events to
//! [`MasterDetail::focus_moved`] and [`MasterDetail::committed`]. The result is a typed
//! [`Outcome::Follow`] request. The owner translates that request into its application effect and,
//! when following projects a detail cursor, `Effects::remember`; this component never mutates an
//! application store. The owner also appends [`MasterDetail::links`] from `Screen::links`.
//!
//! The optional route-family rule that makes a bottom band reachable only from the master is
//! [`BandReach::MasterOnly`]. It is opt-in; ordinary toolbar/band geometry is otherwise untouched.

use super::frame::Budget;
use super::machine::{Canon, Cx, FocusKey, GroupId, Host, LogicalState};
use super::screen::{
    At, Dir, DrawFrame, EdgeRule, Focusable, GroupSpec, Link, Part, Placed, Seat, Step,
};
use super::Rect;

/// Which side of the detail region holds the master.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MasterSide {
    Left,
    Right,
}

impl MasterSide {
    /// The direction from the master through its return door.
    pub const fn toward_detail(self) -> Dir {
        match self {
            Self::Left => Dir::Right,
            Self::Right => Dir::Left,
        }
    }
}

/// When changing the master selection asks the owner to update the detail region.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Follow {
    /// Every `FocusMoved` whose destination is the master requests a follow.
    Live,
    /// Only activation/commit while focused in the master requests a follow.
    OnCommit,
}

/// The route-family action-band exception from spec §10.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BandReach {
    /// Preserve both children's normal group edge policies.
    Normal,
    /// Block the detail region on `dir` and declare a master-to-band link on that direction.
    MasterOnly { group: GroupId, dir: Dir },
}

/// Stable identities for the two semantic groups.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MasterDetailGroups {
    pub master: GroupId,
    /// Geometric fallback when there is no live return door.
    pub detail: GroupId,
}

/// Caller-authored child frames. Children remain responsible for making their `place` geometry
/// describe what they draw in these frames, as every `Part` is under the §7.1 contract.
#[derive(Clone, Copy, Debug)]
pub struct MasterDetailLayout {
    pub master: Rect,
    pub detail: Rect,
}

/// Immutable behavior policy, encoded explicitly so replay/state owners need not hash booleans or
/// enum representations by accident.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MasterDetailPolicy {
    pub side: MasterSide,
    pub follow: Follow,
    pub band: BandReach,
}

impl MasterDetailPolicy {
    pub const fn new(side: MasterSide, follow: Follow) -> Self {
        Self {
            side,
            follow,
            band: BandReach::Normal,
        }
    }

    pub const fn with_band(mut self, band: BandReach) -> Self {
        self.band = band;
        self
    }
}

/// The complete logical state owned by the component. `door_from` is a group identity only;
/// the engine chooses that group's remembered element when the return link is followed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MasterDetailState {
    policy: MasterDetailPolicy,
    door_from: Option<GroupId>,
}

impl MasterDetailState {
    pub const fn new(policy: MasterDetailPolicy) -> Self {
        Self {
            policy,
            door_from: None,
        }
    }

    pub const fn policy(self) -> MasterDetailPolicy {
        self.policy
    }

    pub const fn door_from(self) -> Option<GroupId> {
        self.door_from
    }
}

impl LogicalState for MasterDetailState {
    fn write(&self, c: &mut Canon) {
        c.discriminant(match self.policy.side {
            MasterSide::Left => 0,
            MasterSide::Right => 1,
        });
        c.discriminant(match self.policy.follow {
            Follow::Live => 0,
            Follow::OnCommit => 1,
        });
        match self.policy.band {
            BandReach::Normal => {
                c.discriminant(0);
            }
            BandReach::MasterOnly { group, dir } => {
                c.discriminant(1).u32(group.0).discriminant(dir_tag(dir));
            }
        }
        c.option(self.door_from, |c, group| {
            c.u32(group.0);
        });
    }

    fn probe(&self, out: &mut String) {
        use std::fmt::Write as _;
        let _ = write!(out, "master_detail:door={:?}", self.door_from.map(|g| g.0));
    }
}

const fn dir_tag(dir: Dir) -> u32 {
    match dir {
        Dir::Up => 0,
        Dir::Down => 1,
        Dir::Left => 2,
        Dir::Right => 3,
    }
}

/// Why a detail-follow request was raised.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FollowCause {
    Live,
    Commit,
}

/// A typed request for the owner to translate into application and focus-memory effects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FollowRequest<K> {
    pub master: FocusKey<K>,
    pub cause: FollowCause,
}

/// Result of feeding an owner event through the component's policy.
#[must_use]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome<K> {
    None,
    Follow(FollowRequest<K>),
}

/// Semantic ownership of an element, including an element removed from visible membership.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Region {
    Master,
    Detail,
}

/// Caller-owned identity classification. Return `None` for keys outside this composition.
/// Deleted keys must retain their original region, through typed keys or tombstones in
/// `Cx::views`. Implementations must not keep current or remembered focus of their own.
pub trait KeyRegion<H: Host> {
    fn region(&self, key: &H::Elem, cx: &Cx<'_, H>) -> Option<Region>;
}

/// Two library parts composed as a master and a detail region, with stable key ownership.
pub struct MasterDetail<M, D, R> {
    pub master: M,
    pub detail: D,
    layout: MasterDetailLayout,
    groups: MasterDetailGroups,
    state: MasterDetailState,
    key_region: R,
}

impl<M, D, R> MasterDetail<M, D, R> {
    pub const fn new(
        master: M,
        detail: D,
        layout: MasterDetailLayout,
        groups: MasterDetailGroups,
        policy: MasterDetailPolicy,
        key_region: R,
    ) -> Self {
        Self {
            master,
            detail,
            layout,
            groups,
            state: MasterDetailState::new(policy),
            key_region,
        }
    }

    /// Restore the component's own group-only door and immutable policy.
    pub const fn with_state(mut self, state: MasterDetailState) -> Self {
        self.state = state;
        self
    }

    pub const fn state(&self) -> &MasterDetailState {
        &self.state
    }

    pub const fn layout(&self) -> MasterDetailLayout {
        self.layout
    }

    pub const fn groups_config(&self) -> MasterDetailGroups {
        self.groups
    }

    /// Dynamic links to append from the containing screen's `Screen::links` implementation.
    ///
    /// With no recorded door, the configured detail group is the explicit fallback. A stale
    /// `door_from` is intentionally emitted: `FocusEngine` validates the destination against the
    /// screen's current groups and, if it is absent or empty, falls through to the master's edge
    /// rule; normal geometric placement then reaches the detail region.
    pub fn links(&self, out: &mut Vec<Link>) {
        let policy = self.state.policy;
        let to = self.state.door_from.unwrap_or(self.groups.detail);
        if to != self.groups.master {
            out.push(Link {
                from: self.groups.master,
                dir: policy.side.toward_detail(),
                to,
            });
        }
        if let BandReach::MasterOnly { group, dir } = policy.band {
            debug_assert!(
                dir != policy.side.toward_detail(),
                "master/detail return and master-only band links overlap"
            );
            out.push(Link {
                from: self.groups.master,
                dir,
                to: group,
            });
        }
    }

    /// Observe a delivered `FocusMoved`. The owner supplies group identities resolved against
    /// the whole screen, which is how an external toolbar can be recorded without storing its key.
    pub fn focus_moved<K: Copy>(
        &mut self,
        from_group: Option<GroupId>,
        to_group: Option<GroupId>,
        to: FocusKey<K>,
    ) -> Outcome<K> {
        if to_group != Some(self.groups.master) {
            return Outcome::None;
        }
        if from_group != Some(self.groups.master) {
            self.state.door_from = from_group.filter(|g| *g != self.groups.master);
        }
        match self.state.policy.follow {
            Follow::Live => Outcome::Follow(FollowRequest {
                master: to,
                cause: FollowCause::Live,
            }),
            Follow::OnCommit => Outcome::None,
        }
    }

    /// Observe activation or press commit of `key`. A screen calls this for either activation
    /// spelling after resolving the event's element/press identity to the engine-owned key.
    pub fn committed<K: Copy>(&self, group: Option<GroupId>, key: FocusKey<K>) -> Outcome<K> {
        if group == Some(self.groups.master) && self.state.policy.follow == Follow::OnCommit {
            Outcome::Follow(FollowRequest {
                master: key,
                cause: FollowCause::Commit,
            })
        } else {
            Outcome::None
        }
    }
}

impl<H, M, D, R> Focusable<H> for MasterDetail<M, D, R>
where
    H: Host,
    M: Part<H>,
    D: Part<H>,
    R: KeyRegion<H>,
{
    fn groups(&self, cx: &Cx<'_, H>, out: &mut Vec<GroupSpec>) {
        let master_start = out.len();
        self.master.groups(cx, out);
        let master_end = out.len();
        for spec in &mut out[master_start..master_end] {
            if spec.id == self.groups.master {
                spec.seat = Seat::ProjectedFrom(self.groups.detail);
            }
        }

        let detail_start = out.len();
        self.detail.groups(cx, out);
        if let BandReach::MasterOnly { dir, .. } = self.state.policy.band {
            let edge = dir_index(dir);
            for spec in &mut out[detail_start..] {
                spec.edge[edge] = EdgeRule::Stop;
            }
        }

        debug_assert!(out[master_start..master_end]
            .iter()
            .any(|s| s.id == self.groups.master));
        debug_assert!(out[detail_start..]
            .iter()
            .any(|s| s.id == self.groups.detail));
    }

    fn group_of(&self, key: &H::Elem, cx: &Cx<'_, H>) -> Option<GroupId> {
        self.master
            .group_of(key, cx)
            .or_else(|| self.detail.group_of(key, cx))
    }

    fn neighbour(&self, key: FocusKey<H::Elem>, dir: Dir, cx: &Cx<'_, H>) -> Step<H::Elem> {
        if self.master.group_of(&key.elem, cx).is_some() {
            self.master.neighbour(key, dir, cx)
        } else {
            self.detail.neighbour(key, dir, cx)
        }
    }

    fn place(&self, key: &H::Elem, cx: &Cx<'_, H>, at: At) -> Option<Placed> {
        self.master
            .place(key, cx, at)
            .or_else(|| self.detail.place(key, cx, at))
    }

    fn reconcile(&self, want: FocusKey<H::Elem>, cx: &Cx<'_, H>) -> FocusKey<H::Elem> {
        let (own, other): (&dyn Part<H>, &dyn Part<H>) =
            match self.key_region.region(&want.elem, cx) {
                Some(Region::Master) => (&self.master, &self.detail),
                Some(Region::Detail) => (&self.detail, &self.master),
                None => return want,
            };
        for part in [own, other] {
            let candidate = part.reconcile(want, cx);
            if candidate.entry == want.entry
                && part.group_of(&candidate.elem, cx).is_some()
                && part.place(&candidate.elem, cx, At::SpringTarget).is_some()
            {
                return candidate;
            }
        }
        // Focusable cannot express no destination. Preserve the request if neither child
        // can place a fallback; the owner must handle an entirely empty surface's lifecycle.
        want
    }

    fn seat(&self, group: GroupId, from: Placed, cx: &Cx<'_, H>) -> FocusKey<H::Elem> {
        if owns_group::<H, _>(&self.master, group, cx) {
            // The engine provides the detail group's remembered placement; the caller's master
            // Part projects from its index and may read data through Cx::views.
            self.master.seat(group, from, cx)
        } else {
            self.detail.seat(group, from, cx)
        }
    }
}

impl<H, M, D, R> Part<H> for MasterDetail<M, D, R>
where
    H: Host,
    M: Part<H>,
    D: Part<H>,
    R: KeyRegion<H>,
{
    fn prepare(&mut self, budget: &mut Budget, cx: &Cx<'_, H>) {
        self.master.prepare(budget, cx);
        self.detail.prepare(budget, cx);
    }

    fn draw(&mut self, frame: &mut DrawFrame<'_, H>, _rect: Rect) {
        self.master.draw(frame, self.layout.master);
        self.detail.draw(frame, self.layout.detail);
    }
}

fn owns_group<H, P>(part: &P, group: GroupId, cx: &Cx<'_, H>) -> bool
where
    H: Host,
    P: Part<H>,
{
    let mut groups = Vec::new();
    part.groups(cx, &mut groups);
    groups.iter().any(|spec| spec.id == group)
}

const fn dir_index(dir: Dir) -> usize {
    match dir {
        Dir::Up => 0,
        Dir::Down => 1,
        Dir::Left => 2,
        Dir::Right => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::fixture::{FixtureFx, FixtureHost, FixtureMeasure, FixtureView, FixtureViews};
    use crate::ui::focus::{FocusEngine, Outcome as FocusOutcome};
    use crate::ui::machine::{
        Effects, EntryId, FocusRead, Fx, Handled, InputOwner, InstanceId, Machine, MachineId,
        PressRead, Tick,
    };
    use crate::ui::present::Present;
    use crate::ui::screen::{AxisMask, ElemKind, GroupKind};
    use crate::ui::Painter;

    const ENTRY: EntryId = EntryId(0x8a7c_1101);
    const MASTER: GroupId = GroupId(0xc0de_4107);
    const DETAIL: GroupId = GroupId(0x91af_5e23);
    const TOOLBAR: GroupId = GroupId(0xe771_002d);
    const BAND: GroupId = GroupId(0xbad0_77e5);
    const OWNER: InputOwner = InputOwner::Entry(ENTRY);

    const M0: u32 = 0xa100_0011;
    const M1: u32 = 0xa100_0022;
    const M2: u32 = 0xa100_0033;
    const D0: u32 = 0xd200_0011;
    const D1: u32 = 0xd200_0022;
    const T0: u32 = 0x7100_0011;
    const T1: u32 = 0x7100_0022;
    const T2: u32 = 0x7100_0033;
    const B0: u32 = 0xb400_0011;

    fn rect(x: f32, y: f32) -> Rect {
        Rect::new(x, y, 80.0, 80.0)
    }

    fn same_rect(a: Rect, b: Rect) -> bool {
        (a.x, a.y, a.w, a.h) == (b.x, b.y, b.w, b.h)
    }

    struct ProbePart {
        group: GroupId,
        entry: EntryId,
        kind: GroupKind,
        seat: Seat,
        extent: Rect,
        elems: Vec<(u32, Rect)>,
        project_from_view: bool,
        project_from_index: bool,
        prepared: u32,
        drawn: Vec<Rect>,
        unplaceable: Option<u32>,
    }

    impl ProbePart {
        fn new(
            group: GroupId,
            kind: GroupKind,
            seat: Seat,
            extent: Rect,
            elems: &[(u32, Rect)],
        ) -> Self {
            Self {
                group,
                entry: ENTRY,
                kind,
                seat,
                extent,
                elems: elems.to_vec(),
                project_from_view: false,
                project_from_index: false,
                prepared: 0,
                drawn: Vec::new(),
                unplaceable: None,
            }
        }

        fn projected(mut self) -> Self {
            self.project_from_view = true;
            self
        }

        fn key(&self, i: usize) -> FocusKey<u32> {
            FocusKey {
                entry: self.entry,
                elem: self.elems[i].0,
            }
        }

        fn index(&self, elem: u32) -> Option<usize> {
            self.elems.iter().position(|(key, _)| *key == elem)
        }
    }

    impl Focusable<FixtureHost> for ProbePart {
        fn groups(&self, _cx: &Cx<'_, FixtureHost>, out: &mut Vec<GroupSpec>) {
            out.push(GroupSpec {
                id: self.group,
                kind: self.kind,
                seat: self.seat,
                reachable: AxisMask::BOTH,
                edge: [EdgeRule::Geometric; 4],
                extent: self.extent,
                len: self.elems.len(),
                elem: ElemKind::Control,
            });
        }

        fn group_of(&self, key: &u32, _cx: &Cx<'_, FixtureHost>) -> Option<GroupId> {
            self.index(*key).map(|_| self.group)
        }

        fn neighbour(&self, key: FocusKey<u32>, dir: Dir, _cx: &Cx<'_, FixtureHost>) -> Step<u32> {
            let Some(i) = self.index(key.elem) else {
                return Step::Edge;
            };
            let delta = match (self.kind, dir) {
                (GroupKind::Column, Dir::Up) | (GroupKind::Row { .. }, Dir::Left) => -1,
                (GroupKind::Column, Dir::Down) | (GroupKind::Row { .. }, Dir::Right) => 1,
                _ => return Step::Edge,
            };
            let next = i as isize + delta;
            if (0..self.elems.len() as isize).contains(&next) {
                Step::Move(self.key(next as usize))
            } else {
                Step::Edge
            }
        }

        fn place(&self, key: &u32, _cx: &Cx<'_, FixtureHost>, _at: At) -> Option<Placed> {
            if self.unplaceable == Some(*key) {
                return None;
            }
            let i = self.index(*key)?;
            let r = self.elems[i].1;
            Some(Placed {
                rect: r,
                rest_rect: r,
                clip: self.extent,
                index: Some(i as u32),
            })
        }

        fn reconcile(&self, want: FocusKey<u32>, _cx: &Cx<'_, FixtureHost>) -> FocusKey<u32> {
            if self.index(want.elem).is_some() || self.elems.is_empty() {
                want
            } else {
                self.key(0)
            }
        }

        fn seat(&self, _group: GroupId, from: Placed, cx: &Cx<'_, FixtureHost>) -> FocusKey<u32> {
            if self.project_from_index {
                return self.key((from.index.unwrap_or(0) as usize).min(self.elems.len() - 1));
            }
            if self.project_from_view {
                let i = cx.views.store.items.first().copied().unwrap_or(0) as usize;
                return self.key(i.min(self.elems.len() - 1));
            }
            let cy = from.rect.cy();
            let i = self
                .elems
                .iter()
                .enumerate()
                .min_by(|(_, a), (_, b)| {
                    (a.1.cy() - cy)
                        .abs()
                        .partial_cmp(&(b.1.cy() - cy).abs())
                        .unwrap()
                })
                .map(|(i, _)| i)
                .unwrap_or(0);
            self.key(i)
        }
    }

    impl Part<FixtureHost> for ProbePart {
        fn prepare(&mut self, _budget: &mut Budget, _cx: &Cx<'_, FixtureHost>) {
            self.prepared += 1;
        }

        fn draw(&mut self, _frame: &mut DrawFrame<'_, FixtureHost>, rect: Rect) {
            self.drawn.push(rect);
        }
    }

    #[test]
    fn removed_detail_key_reconciles_to_surviving_detail() {
        let measure = FixtureMeasure;
        let view = FixtureView::default();
        let cx = cx(&measure, &view);
        let mut md = component(MasterSide::Left, Follow::Live);
        let mut engine = FocusEngine::new();
        engine.set(
            OWNER,
            md.detail.key(1),
            Some(DETAIL),
            crate::ui::screen::By::Restore,
        );
        md.detail.elems.retain(|(key, _)| *key != D1);
        let (_, to) = moved(engine.reconcile(OWNER, &md, &cx));
        assert_eq!(to.elem, D0);
        assert_eq!(engine.current_group(OWNER), Some(DETAIL));
    }

    #[test]
    fn removed_master_key_reconciles_to_surviving_master() {
        let measure = FixtureMeasure;
        let view = FixtureView::default();
        let cx = cx(&measure, &view);
        let mut md = component(MasterSide::Left, Follow::Live);
        let mut engine = FocusEngine::new();
        engine.set(
            OWNER,
            md.master.key(1),
            Some(MASTER),
            crate::ui::screen::By::Restore,
        );
        md.master.elems.retain(|(key, _)| *key != M1);
        let (_, to) = moved(engine.reconcile(OWNER, &md, &cx));
        assert_eq!(to.elem, M0);
        assert_eq!(engine.current_group(OWNER), Some(MASTER));
    }

    #[test]
    fn reconciliation_uses_other_region_only_when_own_fallback_cannot_place() {
        let measure = FixtureMeasure;
        let view = FixtureView::default();
        let cx = cx(&measure, &view);
        for removed_region in [Region::Master, Region::Detail] {
            for empty in [false, true] {
                let mut md = component(MasterSide::Left, Follow::Live);
                let (own, group, expected, expected_group) = match removed_region {
                    Region::Master => (&mut md.master, MASTER, D0, DETAIL),
                    Region::Detail => (&mut md.detail, DETAIL, M0, MASTER),
                };
                let want = own.key(1);
                let mut engine = FocusEngine::new();
                engine.set(OWNER, want, Some(group), crate::ui::screen::By::Restore);
                if empty {
                    own.elems.clear();
                } else {
                    own.elems.truncate(1);
                    own.unplaceable = Some(own.key(0).elem);
                }
                let (_, to) = moved(engine.reconcile(OWNER, &md, &cx));
                assert_eq!(to.elem, expected);
                assert_eq!(engine.current_group(OWNER), Some(expected_group));
            }
        }
    }

    #[test]
    fn reconciliation_preserves_request_when_neither_region_has_a_viable_fallback() {
        let measure = FixtureMeasure;
        let view = FixtureView::default();
        let cx = cx(&measure, &view);
        let mut md = component(MasterSide::Left, Follow::Live);
        let want = md.detail.key(1);
        let mut engine = FocusEngine::new();
        engine.set(OWNER, want, Some(DETAIL), crate::ui::screen::By::Restore);
        md.detail.elems.clear();
        md.master.unplaceable = Some(M0);
        assert_eq!(engine.reconcile(OWNER, &md, &cx), FocusOutcome::Nothing);
        assert_eq!(engine.current(OWNER), Some(want));
    }

    struct FixtureRegions;

    impl KeyRegion<FixtureHost> for FixtureRegions {
        fn region(&self, key: &u32, _cx: &Cx<'_, FixtureHost>) -> Option<Region> {
            // The fixture identity domain includes tombstones regardless of current membership.
            match *key {
                M0 | M1 | M2 => Some(Region::Master),
                D0 | D1 => Some(Region::Detail),
                _ => None,
            }
        }
    }

    type Md = MasterDetail<ProbePart, ProbePart, FixtureRegions>;

    fn component(side: MasterSide, follow: Follow) -> Md {
        component_with_policy(MasterDetailPolicy::new(side, follow))
    }

    fn component_with_policy(policy: MasterDetailPolicy) -> Md {
        let side = policy.side;
        let (master_x, detail_x) = match side {
            MasterSide::Left => (80.0, 520.0),
            MasterSide::Right => (1460.0, 720.0),
        };
        let master_rect = Rect::new(master_x, 180.0, 180.0, 460.0);
        let detail_rect = Rect::new(detail_x, 180.0, 620.0, 460.0);
        let master = ProbePart::new(
            MASTER,
            GroupKind::Column,
            Seat::Nearest,
            master_rect,
            &[
                (M0, rect(master_x, 200.0)),
                (M1, rect(master_x, 320.0)),
                (M2, rect(master_x, 440.0)),
            ],
        )
        .projected();
        let detail = ProbePart::new(
            DETAIL,
            GroupKind::Row { wrap: false },
            Seat::Remembered,
            detail_rect,
            &[
                (D0, rect(detail_x, 250.0)),
                (D1, rect(detail_x + 120.0, 250.0)),
            ],
        );
        MasterDetail::new(
            master,
            detail,
            MasterDetailLayout {
                master: master_rect,
                detail: detail_rect,
            },
            MasterDetailGroups {
                master: MASTER,
                detail: DETAIL,
            },
            policy,
            FixtureRegions,
        )
    }

    fn cx<'a>(measure: &'a FixtureMeasure, view: &'a FixtureView) -> Cx<'a, FixtureHost> {
        Cx {
            views: FixtureViews { store: view },
            tick: Tick::default(),
            measure,
            press: PressRead::default(),
            focus: FocusRead::default(),
            owner: OWNER,
        }
    }

    struct FixtureScreen<'a> {
        toolbar: Option<&'a ProbePart>,
        master_detail: &'a Md,
        band: Option<&'a ProbePart>,
    }

    impl FixtureScreen<'_> {
        fn links(&self) -> Vec<Link> {
            let mut links = Vec::new();
            if self.toolbar.is_some() {
                links.push(Link {
                    from: TOOLBAR,
                    dir: Dir::Down,
                    to: MASTER,
                });
            }
            self.master_detail.links(&mut links);
            links
        }

        fn parts(&self) -> impl Iterator<Item = &ProbePart> {
            self.toolbar
                .into_iter()
                .chain([&self.master_detail.master, &self.master_detail.detail])
                .chain(self.band)
        }
    }

    impl Focusable<FixtureHost> for FixtureScreen<'_> {
        fn groups(&self, cx: &Cx<'_, FixtureHost>, out: &mut Vec<GroupSpec>) {
            if let Some(toolbar) = self.toolbar {
                toolbar.groups(cx, out);
            }
            self.master_detail.groups(cx, out);
            if let Some(band) = self.band {
                band.groups(cx, out);
            }
        }

        fn group_of(&self, key: &u32, cx: &Cx<'_, FixtureHost>) -> Option<GroupId> {
            self.parts().find_map(|part| part.group_of(key, cx))
        }

        fn neighbour(&self, key: FocusKey<u32>, dir: Dir, cx: &Cx<'_, FixtureHost>) -> Step<u32> {
            self.parts()
                .find(|part| part.group_of(&key.elem, cx).is_some())
                .map_or(Step::Edge, |part| part.neighbour(key, dir, cx))
        }

        fn place(&self, key: &u32, cx: &Cx<'_, FixtureHost>, at: At) -> Option<Placed> {
            self.parts().find_map(|part| part.place(key, cx, at))
        }

        fn reconcile(&self, want: FocusKey<u32>, cx: &Cx<'_, FixtureHost>) -> FocusKey<u32> {
            if FixtureRegions.region(&want.elem, cx).is_some() {
                return self.master_detail.reconcile(want, cx);
            }
            self.parts()
                .find(|part| part.group_of(&want.elem, cx).is_some())
                .map_or(want, |part| part.reconcile(want, cx))
        }

        fn seat(&self, group: GroupId, from: Placed, cx: &Cx<'_, FixtureHost>) -> FocusKey<u32> {
            if group == MASTER || group == DETAIL {
                self.master_detail.seat(group, from, cx)
            } else {
                self.parts()
                    .find(|part| part.group == group)
                    .expect("fixture link names a live group")
                    .seat(group, from, cx)
            }
        }
    }

    fn moved(outcome: FocusOutcome<u32>) -> (Option<FocusKey<u32>>, FocusKey<u32>) {
        match outcome {
            FocusOutcome::Moved { from, to, .. } => (from, to),
            other => panic!("expected focus move, got {other:?}"),
        }
    }

    #[test]
    fn master_entry_from_toolbar_projects_the_engine_remembered_detail() {
        let measure = FixtureMeasure;
        let view = FixtureView::default();
        let cx = cx(&measure, &view);
        for side in [MasterSide::Left, MasterSide::Right] {
            let mut md = component(side, Follow::Live);
            md.master.project_from_view = false;
            md.master.project_from_index = true;
            let toolbar = ProbePart::new(TOOLBAR, GroupKind::Row { wrap: false },
                Seat::Remembered, Rect::new(300.0, 20.0, 500.0, 90.0), &[(T0, rect(300.0, 20.0))]);
            let screen = FixtureScreen { toolbar: Some(&toolbar), master_detail: &md, band: None };
            let mut engine = FocusEngine::new();
            engine.set(OWNER, md.detail.key(1), Some(DETAIL), crate::ui::screen::By::Restore);
            engine.set(OWNER, toolbar.key(0), Some(TOOLBAR), crate::ui::screen::By::Restore);
            let (_, to) = moved(engine.move_dir(OWNER, &screen, &screen.links(), Dir::Down, &cx));
            assert_eq!(to, md.master.key(1), "toolbar index zero must not replace the remembered grid index");
            assert_eq!(engine.remembered_for(ENTRY).iter().find(|(g, _)| *g == DETAIL).map(|(_, k)| *k), Some(D1));
            engine.remember_projected(ENTRY, DETAIL, D0);
            engine.set(OWNER, toolbar.key(0), Some(TOOLBAR), crate::ui::screen::By::Restore);
            let (_, to) = moved(engine.move_dir(OWNER, &screen, &screen.links(), Dir::Down, &cx));
            assert_eq!(to, md.master.key(0), "a new engine remember effect is visible on the very next entry");
        }
    }

    #[test]
    fn projected_master_enter_reads_restored_engine_group_memory() {
        let measure = FixtureMeasure;
        let view = FixtureView::default();
        let cx = cx(&measure, &view);
        let mut md = component(MasterSide::Left, Follow::Live);
        md.master.project_from_view = false;
        md.master.project_from_index = true;
        let mut engine = FocusEngine::new();
        engine.restore_remembered(ENTRY, &[(DETAIL, D1)]);
        let (_, to) = moved(engine.enter(OWNER, &md,
            crate::ui::screen::FocusTarget::ContainerGroup(MASTER), None, &cx));
        assert_eq!(to, md.master.key(1));
        assert_eq!(engine.current_group(OWNER), Some(MASTER));
    }

    #[test]
    fn master_projection_reconciles_detail_identity_without_moving_its_cursor() {
        let measure = FixtureMeasure;
        let view = FixtureView::default();
        let cx = cx(&measure, &view);
        for case in ["reordered", "removed", "unplaceable", "empty", "other_entry", "fresh"] {
            let mut md = component(MasterSide::Right, Follow::Live);
            md.master.project_from_view = false;
            md.master.project_from_index = true;
            let toolbar = ProbePart::new(TOOLBAR, GroupKind::Row { wrap: false },
                Seat::Remembered, Rect::new(300.0, 20.0, 500.0, 90.0),
                &[(T0, rect(300.0, 20.0)), (T1, rect(400.0, 20.0)), (T2, rect(500.0, 20.0))]);
            let mut engine = FocusEngine::new();
            if case == "other_entry" {
                let other = EntryId(ENTRY.0 + 1);
                engine.set(InputOwner::Entry(other), FocusKey { entry: other, elem: D1 },
                    Some(DETAIL), crate::ui::screen::By::Restore);
            } else if case != "fresh" {
                engine.set(OWNER, md.detail.key(1), Some(DETAIL), crate::ui::screen::By::Restore);
            }
            match case {
                "reordered" => md.detail.elems.reverse(),
                "removed" => md.detail.elems.truncate(1),
                "unplaceable" => md.detail.unplaceable = Some(D1),
                "empty" => md.detail.elems.clear(),
                _ => (),
            }
            let remembered = engine.remembered_for(ENTRY).into_iter().filter(|(g, _)| *g == DETAIL).collect::<Vec<_>>();
            engine.set(OWNER, toolbar.key(2), Some(TOOLBAR), crate::ui::screen::By::Restore);
            let screen = FixtureScreen { toolbar: Some(&toolbar), master_detail: &md, band: None };
            let (_, to) = moved(engine.move_dir(OWNER, &screen, &screen.links(), Dir::Down, &cx));
            let expected = if matches!(case, "reordered" | "removed") { M0 } else { M2 };
            assert_eq!(to.elem, expected, "{case}: recover/replace source placement, never invent a cursor");
            assert_eq!(engine.remembered_for(ENTRY).into_iter().filter(|(g, _)| *g == DETAIL).collect::<Vec<_>>(), remembered,
                "projection is a read; only an explicit remember effect may change the detail cursor");
        }
    }

    #[test]
    fn projected_entry_uses_the_master_parts_published_view_policy() {
        let measure = FixtureMeasure;
        let view = FixtureView {
            items: vec![2],
            gen: 0,
        };
        let cx = cx(&measure, &view);
        let md = component(MasterSide::Left, Follow::Live);
        let mut groups = Vec::new();
        md.groups(&cx, &mut groups);
        assert_eq!(
            groups.iter().find(|g| g.id == MASTER).unwrap().seat,
            Seat::ProjectedFrom(DETAIL)
        );

        let mut engine = FocusEngine::new();
        engine.set(
            OWNER,
            md.detail.key(0),
            Some(DETAIL),
            crate::ui::screen::By::Restore,
        );
        let (_, to) = moved(engine.move_dir(OWNER, &md, &[], Dir::Left, &cx));
        assert_eq!(
            to,
            md.master.key(2),
            "the projection came from Cx::views, not source geometry"
        );
    }

    #[test]
    fn return_door_remembers_only_the_group_and_engine_seats_its_new_cursor() {
        let measure = FixtureMeasure;
        let view = FixtureView {
            items: vec![1],
            gen: 0,
        };
        let cx = cx(&measure, &view);
        let mut md = component(MasterSide::Left, Follow::Live);
        let toolbar = ProbePart::new(
            TOOLBAR,
            GroupKind::Row { wrap: false },
            Seat::Remembered,
            Rect::new(300.0, 20.0, 500.0, 90.0),
            &[
                (T0, rect(300.0, 20.0)),
                (T1, rect(420.0, 20.0)),
                (T2, rect(540.0, 20.0)),
            ],
        );

        let mut engine = FocusEngine::new();
        engine.set(
            OWNER,
            toolbar.key(1),
            Some(TOOLBAR),
            crate::ui::screen::By::Restore,
        );
        let (_, in_master) = {
            let screen = FixtureScreen {
                toolbar: Some(&toolbar),
                master_detail: &md,
                band: None,
            };
            moved(engine.move_dir(OWNER, &screen, &screen.links(), Dir::Down, &cx))
        };
        let follow = md.focus_moved(Some(TOOLBAR), Some(MASTER), in_master);
        assert!(matches!(follow, Outcome::Follow(_)));
        assert_eq!(md.state().door_from(), Some(TOOLBAR));

        // Change only the engine's remembered toolbar cursor while focus remains in master.
        engine.remember_projected(ENTRY, TOOLBAR, T2);
        let screen = FixtureScreen {
            toolbar: Some(&toolbar),
            master_detail: &md,
            band: None,
        };
        let (_, returned) =
            moved(engine.move_dir(OWNER, &screen, &screen.links(), Dir::Right, &cx));
        assert_eq!(
            returned,
            toolbar.key(2),
            "the component stored no toolbar key of its own"
        );
    }

    #[test]
    fn absent_or_removed_door_uses_the_detail_fallback() {
        let measure = FixtureMeasure;
        let view = FixtureView::default();
        let cx = cx(&measure, &view);

        for stale in [false, true] {
            let mut md = component(MasterSide::Left, Follow::Live);
            if stale {
                let _ = md.focus_moved(Some(TOOLBAR), Some(MASTER), md.master.key(0));
                assert_eq!(md.state().door_from(), Some(TOOLBAR));
            }
            let mut engine = FocusEngine::new();
            engine.set(
                OWNER,
                md.master.key(0),
                Some(MASTER),
                crate::ui::screen::By::Restore,
            );
            let (_, to) = moved(engine.move_dir(
                OWNER,
                &md,
                &{
                    let mut links = Vec::new();
                    md.links(&mut links);
                    links
                },
                Dir::Right,
                &cx,
            ));
            assert_eq!(md.group_of(&to.elem, &cx), Some(DETAIL));
        }
    }

    #[test]
    fn return_door_tracks_the_inward_direction_on_both_master_sides() {
        let measure = FixtureMeasure;
        let view = FixtureView::default();
        let cx = cx(&measure, &view);

        for (side, dir) in [
            (MasterSide::Left, Dir::Right),
            (MasterSide::Right, Dir::Left),
        ] {
            let mut md = component(side, Follow::OnCommit);
            let _ = md.focus_moved(Some(DETAIL), Some(MASTER), md.master.key(0));
            let mut links = Vec::new();
            md.links(&mut links);
            assert!(links.contains(&Link {
                from: MASTER,
                dir,
                to: DETAIL,
            }));

            let mut engine = FocusEngine::new();
            engine.set(
                OWNER,
                md.master.key(0),
                Some(MASTER),
                crate::ui::screen::By::Restore,
            );
            let (_, to) = moved(engine.move_dir(OWNER, &md, &links, dir, &cx));
            assert_eq!(md.group_of(&to.elem, &cx), Some(DETAIL));
        }
    }

    struct FixtureOwner;

    impl Machine<FixtureHost> for FixtureOwner {
        type Ev = FollowRequest<u32>;

        fn step(
            &mut self,
            request: &Self::Ev,
            _cx: &Cx<'_, FixtureHost>,
            fx: &mut Effects<'_, FixtureHost>,
        ) -> Handled {
            let projected = if request.master.elem == M1 { D1 } else { D0 };
            fx.remember(DETAIL, projected);
            fx.push(Fx::App(FixtureFx::StoreAdd(request.master.elem)));
            Handled::Yes
        }
    }

    #[test]
    fn live_follows_focus_while_on_commit_waits_for_activation_and_owner_emits_effects() {
        let measure = FixtureMeasure;
        let view = FixtureView::default();
        let cx = cx(&measure, &view);

        for (follow, expected_move) in [(Follow::Live, true), (Follow::OnCommit, false)] {
            let mut md = component(MasterSide::Left, follow);
            let mut engine = FocusEngine::new();
            engine.set(
                OWNER,
                md.master.key(0),
                Some(MASTER),
                crate::ui::screen::By::Restore,
            );
            let (from, to) = moved(engine.move_dir(OWNER, &md, &[], Dir::Down, &cx));
            let outcome = md.focus_moved(
                from.and_then(|key| md.group_of(&key.elem, &cx)),
                md.group_of(&to.elem, &cx),
                to,
            );
            assert_eq!(matches!(outcome, Outcome::Follow(_)), expected_move);

            let outcome = if follow == Follow::OnCommit {
                md.committed(Some(MASTER), to)
            } else {
                outcome
            };
            let Outcome::Follow(request) = outcome else {
                panic!("policy should have produced one follow request")
            };
            assert_eq!(
                request.cause,
                if follow == Follow::Live {
                    FollowCause::Live
                } else {
                    FollowCause::Commit
                }
            );

            // A fixture screen owner translates the typed request into the real generic effect
            // vocabulary: engine memory plus an opaque application/store command.
            let mut emitted = Vec::new();
            let mut present = Present::new();
            let mut fx = Effects::new(
                &mut emitted,
                MachineId::Instance(InstanceId(0xf100_0099)),
                &mut present,
            );
            let mut owner = FixtureOwner;
            assert_eq!(owner.step(&request, &cx, &mut fx), Handled::Yes);
            assert!(matches!(
                &emitted[0].fx,
                Fx::Remember { group, elem } if *group == DETAIL && *elem == D1
            ));
            assert!(matches!(
                &emitted[1].fx,
                Fx::App(FixtureFx::StoreAdd(elem)) if *elem == M1
            ));
        }
    }

    #[test]
    fn master_only_band_policy_is_opt_in_and_enforced_by_engine_edges_and_links() {
        let measure = FixtureMeasure;
        let view = FixtureView::default();
        let cx = cx(&measure, &view);
        let md = component_with_policy(
            MasterDetailPolicy::new(MasterSide::Left, Follow::Live).with_band(
                BandReach::MasterOnly {
                    group: BAND,
                    dir: Dir::Down,
                },
            ),
        );
        let band = ProbePart::new(
            BAND,
            GroupKind::Row { wrap: false },
            Seat::Remembered,
            Rect::new(80.0, 800.0, 900.0, 100.0),
            &[(B0, rect(80.0, 800.0))],
        );
        let screen = FixtureScreen {
            toolbar: None,
            master_detail: &md,
            band: Some(&band),
        };
        let links = screen.links();

        let mut engine = FocusEngine::new();
        engine.set(
            OWNER,
            md.detail.key(0),
            Some(DETAIL),
            crate::ui::screen::By::Restore,
        );
        assert_eq!(
            engine.move_dir(OWNER, &screen, &links, Dir::Down, &cx),
            FocusOutcome::Nothing
        );

        engine.set(
            OWNER,
            md.master.key(2),
            Some(MASTER),
            crate::ui::screen::By::Restore,
        );
        let (_, to) = moved(engine.move_dir(OWNER, &screen, &links, Dir::Down, &cx));
        assert_eq!(to, band.key(0));

        let normal = component(MasterSide::Left, Follow::Live);
        let mut groups = Vec::new();
        normal.groups(&cx, &mut groups);
        assert_eq!(
            groups.iter().find(|g| g.id == DETAIL).unwrap().edge[1],
            EdgeRule::Geometric
        );
    }

    #[test]
    fn child_group_place_prepare_and_explicit_draw_geometry_are_delegated() {
        let measure = FixtureMeasure;
        let view = FixtureView::default();
        let cx = cx(&measure, &view);
        let mut md = component(MasterSide::Right, Follow::Live);
        let layout = md.layout();

        let mut groups = Vec::new();
        md.groups(&cx, &mut groups);
        assert_eq!(
            groups.iter().map(|g| g.id).collect::<Vec<_>>(),
            vec![MASTER, DETAIL]
        );
        assert_eq!(
            md.place(&M2, &cx, At::Drawn).unwrap().rect.x,
            md.master.elems[2].1.x
        );
        assert_eq!(
            md.place(&D1, &cx, At::SpringTarget).unwrap().rect.x,
            md.detail.elems[1].1.x
        );

        let mut budget = Budget::new();
        md.prepare(&mut budget, &cx);
        assert_eq!((md.master.prepared, md.detail.prepared), (1, 1));

        let mut frame = DrawFrame::new(&cx, Painter::root());
        md.draw(&mut frame, Rect::new(999.0, 999.0, 1.0, 1.0));
        assert_eq!(md.master.drawn.len(), 1);
        assert_eq!(md.detail.drawn.len(), 1);
        assert!(same_rect(md.master.drawn[0], layout.master));
        assert!(same_rect(md.detail.drawn[0], layout.detail));
    }

    #[test]
    fn canonical_state_distinguishes_policy_and_opaque_group_door() {
        let mut left = component(MasterSide::Left, Follow::Live);
        let right = component(MasterSide::Right, Follow::Live);
        assert_ne!(left.state().hash(), right.state().hash());
        let before = left.state().hash();
        let _ = left.focus_moved(Some(TOOLBAR), Some(MASTER), left.master.key(0));
        assert_ne!(before, left.state().hash());
        assert_eq!(left.state().door_from(), Some(TOOLBAR));
    }
}
