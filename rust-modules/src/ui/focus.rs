//! The focus ENGINE (restructure spec §7.3), owned by `Input`: THE current `FocusKey` per input
//! scope and every group's remembered cursor — the only focus state there is; screens keep no
//! copy and read `cx.focus`. It never mutates a screen: it asks the owner's `Focusable` (the
//! §7.1 query protocol) and answers with a `FocusMoved` the OWNER acts on (scrolling the target
//! into view is the owner's, on that event).
//!
//! Movement, in the spec's order: (1) the owner's `step` had first refusal (the dispatcher's
//! job, before this is asked); (2) the current group's `neighbour`; (3) at an edge, a declared
//! `Link` wins, else the group's `EdgeRule` for that side — `Geometric` is the search for the
//! destination group, nearest by axis distance weighted 3:1 against orthogonal offset from the
//! current element's PLACED rect (`At::SpringTarget`: two fast presses resolve against where
//! focus is GOING) over every group's extent, excluding groups whose `reachable` mask omits the
//! axis; (4) landing by the destination's `Seat` — `Nearest` and `Projected` through the
//! container's own `seat` (`column_near_x`'s contract), `Remembered` unconditionally,
//! `RememberedNear{rows}` only within `rows` element-widths, `First` at the extent's head;
//! (5) the engine records the move and emits it; (6) after a landing, the owner's pure
//! `reconcile(want)` — if the answer differs, `FocusMoved{by: Reconcile}` (the Slot→Item
//! promotion is exactly this); (7) scope: the input owner's groups only.
#![allow(dead_code)] // phase 3b: the product's LegacyPage keeps the engine inert until 5b

use std::hash::Hash;

use super::machine::{Canon, EntryId, FocusKey, GroupId, Host, InputOwner};
use super::screen::{At, By, Dir, EdgeRule, ElemKind, Focusable, FocusTarget, GroupSpec, Link, Placed, Seat, Step};
use super::Rect;
use super::machine::Cx;

/// What a direction did (§7.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome<K> {
    /// Focus moved; the owner hears `FocusMoved{from, to, by}`.
    Moved {
        from: Option<FocusKey<K>>,
        to: FocusKey<K>,
        by: By,
    },
    /// The group's edge rule answered something other than a move: the dispatcher acts on it.
    Edge(EdgeRule),
    /// Nothing to do (no focus, no groups, a `Stop`).
    Nothing,
}

/// The current key per input scope plus every group's remembered cursor — logical state
/// (hashed, restored, recorded). Explicitly ordered sequences, never a map (§5.4).
pub struct FocusEngine<K> {
    scopes: Vec<(InputOwner, FocusKey<K>, Option<GroupId>)>,
    remembered: Vec<((EntryId, GroupId), K)>,
    /// The engine's last-resort fallback (group policy → first) fired: logged once.
    fell_back: bool,
}

impl<K: Copy + Eq + Hash> Default for FocusEngine<K> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K: Copy + Eq + Hash> FocusEngine<K> {
    pub fn new() -> Self {
        Self {
            scopes: Vec::new(),
            remembered: Vec::new(),
            fell_back: false,
        }
    }

    /// The current focus of a scope.
    pub fn current(&self, owner: InputOwner) -> Option<FocusKey<K>> {
        self.scopes.iter().find(|(o, _, _)| *o == owner).map(|(_, k, _)| *k)
    }

    /// The group the current focus was seated in, when known (what a recording carries so a
    /// replay can restore the remembered cursor too).
    pub fn current_group(&self, owner: InputOwner) -> Option<GroupId> {
        self.scopes.iter().find(|(o, _, _)| *o == owner).and_then(|(_, _, g)| *g)
    }

    fn remember(&mut self, key: FocusKey<K>, group: GroupId) {
        let id = (key.entry, group);
        match self.remembered.iter_mut().find(|(g, _)| *g == id) {
            Some((_, k)) => *k = key.elem,
            None => self.remembered.push((id, key.elem)),
        }
    }

    fn remembered_in(&self, entry: EntryId, group: GroupId) -> Option<K> {
        self.remembered
            .iter()
            .find(|((e, g), _)| *e == entry && *g == group)
            .map(|(_, k)| *k)
    }

    /// Park focus (a pointer hover, a restore, a reconcile): the engine records it and answers
    /// the move for the owner to act on. `group` is the key's group when known (remembered).
    pub fn set(&mut self, owner: InputOwner, key: FocusKey<K>, group: Option<GroupId>, by: By) -> Outcome<K> {
        let from = self.current(owner);
        if from == Some(key) {
            return Outcome::Nothing;
        }
        match self.scopes.iter_mut().find(|(o, _, _)| *o == owner) {
            Some((_, k, g)) => {
                *k = key;
                *g = group;
            }
            None => self.scopes.push((owner, key, group)),
        }
        if let Some(g) = group {
            self.remember(key, g);
        }
        Outcome::Moved { from, to: key, by }
    }

    /// Drop a scope's current focus (a replay feeding "no focus").
    pub fn clear(&mut self, owner: InputOwner) {
        self.scopes.retain(|(o, _, _)| *o != owner);
    }

    /// An entry left for good: its scope and cursors go with it.
    pub fn forget(&mut self, entry: EntryId) {
        self.scopes
            .retain(|(o, k, _)| !(k.entry == entry || *o == InputOwner::Entry(entry)));
        self.remembered.retain(|((e, _), _)| *e != entry);
    }

    /// Seat focus on entering a screen (§3.4 `Enter`): a fresh enter lands by the target — an
    /// element outright, or a container group's `Seat` policy from the extent's head; a restore
    /// asks `reconcile` about the return key first.
    pub fn enter<H: Host<Elem = K>>(
        &mut self,
        owner: InputOwner,
        f: &dyn Focusable<H>,
        target: FocusTarget<K>,
        restored: Option<FocusKey<K>>,
        cx: &Cx<'_, H>,
    ) -> Outcome<K> {
        let mut groups = Vec::new();
        f.groups(cx, &mut groups);
        let (key, by) = match (restored, target) {
            (Some(want), _) => (f.reconcile(want, cx), By::Restore),
            (None, FocusTarget::Elem(k)) => (k, By::Restore),
            (None, FocusTarget::ContainerGroup(g)) => {
                let Some(spec) = groups
                    .iter()
                    .find(|s| s.id == g)
                    .or_else(|| groups.iter().find(|s| s.len > 0))
                else {
                    return Outcome::Nothing;
                };
                if spec.len == 0 {
                    return Outcome::Nothing;
                }
                let from = head_of(spec.extent);
                (self.seat_in(f, spec, from, cx), By::Restore)
            }
        };
        let group = f.group_of(&key.elem, cx);
        self.set(owner, key, group, by)
    }

    /// Land in `spec` by its `Seat` policy from a source placement.
    fn seat_in<H: Host<Elem = K>>(&self, f: &dyn Focusable<H>, spec: &GroupSpec, from: Placed, cx: &Cx<'_, H>) -> FocusKey<K> {
        let entry_of = |k: FocusKey<K>| k.entry;
        let projected = f.seat(spec.id, from, cx);
        match spec.seat {
            Seat::Nearest | Seat::Projected => projected,
            Seat::First => f.seat(spec.id, head_of(spec.extent), cx),
            Seat::Remembered => match self.remembered_in(entry_of(projected), spec.id) {
                Some(k) => FocusKey {
                    entry: entry_of(projected),
                    elem: k,
                },
                None => projected,
            },
            Seat::RememberedNear { rows } => {
                let entry = entry_of(projected);
                match self.remembered_in(entry, spec.id) {
                    Some(k) => {
                        let rem = FocusKey { entry, elem: k };
                        let near = f
                            .place(&rem.elem, cx, At::SpringTarget)
                            .map(|p| {
                                let dx = (p.rest_rect.cx() - from.rect.cx()).abs();
                                let dy = (p.rest_rect.cy() - from.rect.cy()).abs();
                                // `rows` elements away, measured in the element's own size
                                // with half an element of slack for the pitch's gap
                                let slack = rows as f32 + 0.5;
                                dx <= slack * p.rest_rect.w.max(1.0) || dy <= slack * p.rest_rect.h.max(1.0)
                            })
                            .unwrap_or(false);
                        if near {
                            rem
                        } else {
                            projected
                        }
                    }
                    None => projected,
                }
            }
        }
    }

    /// A direction the owner declined (§7.3 steps 2–5).
    pub fn move_dir<H: Host<Elem = K>>(
        &mut self,
        owner: InputOwner,
        f: &dyn Focusable<H>,
        links: &[Link],
        dir: Dir,
        cx: &Cx<'_, H>,
    ) -> Outcome<K> {
        let mut groups = Vec::new();
        f.groups(cx, &mut groups);
        let Some(cur) = self.current(owner) else {
            // no focus yet: the first group's seat is the last resort (logged once by the caller)
            let Some(spec) = groups.iter().find(|s| s.len > 0) else {
                return Outcome::Nothing;
            };
            self.fell_back = true;
            let key = self.seat_in(f, spec, head_of(spec.extent), cx);
            return self.set(owner, key, Some(spec.id), By::Dir);
        };
        let Some(cur_group) = f.group_of(&cur.elem, cx) else {
            return Outcome::Nothing;
        };
        // (2) inside the group
        if let Step::Move(k) = f.neighbour(cur, dir, cx) {
            return self.set(owner, k, Some(cur_group), By::Dir);
        }
        // (3) a declared Link wins…
        if let Some(l) = links.iter().find(|l| l.from == cur_group && l.dir == dir) {
            if let Some(spec) = groups.iter().find(|s| s.id == l.to && s.len > 0) {
                let from = f.place(&cur.elem, cx, At::SpringTarget).unwrap_or(Placed {
                    rect: spec.extent,
                    rest_rect: spec.extent,
                    clip: Rect::FULL,
                    index: None,
                });
                let key = self.seat_in(f, spec, from, cx);
                return self.set(owner, key, Some(spec.id), By::Dir);
            }
        }
        // …else the group's edge rule for that side
        let Some(cur_spec) = groups.iter().find(|s| s.id == cur_group) else {
            return Outcome::Nothing;
        };
        match cur_spec.edge[side(dir)] {
            EdgeRule::Stop => Outcome::Nothing,
            EdgeRule::Screen => Outcome::Edge(EdgeRule::Screen),
            EdgeRule::Nav(k) => Outcome::Edge(EdgeRule::Nav(k)),
            EdgeRule::Geometric => {
                let Some(from) = f.place(&cur.elem, cx, At::SpringTarget) else {
                    return Outcome::Nothing;
                };
                let Some(dest) = geometric(&groups, cur_group, from.rect, dir) else {
                    return Outcome::Nothing;
                };
                let key = self.seat_in(f, dest, from, cx);
                self.set(owner, key, Some(dest.id), By::Dir)
            }
        }
    }

    /// After a landing and before draw (§7.3 step 6): the owner's pure `reconcile` on the
    /// current key; a different answer is a `Reconcile` move.
    pub fn reconcile<H: Host<Elem = K>>(&mut self, owner: InputOwner, f: &dyn Focusable<H>, cx: &Cx<'_, H>) -> Outcome<K> {
        let Some(cur) = self.current(owner) else {
            return Outcome::Nothing;
        };
        let want = f.reconcile(cur, cx);
        if want == cur {
            return Outcome::Nothing;
        }
        let group = f.group_of(&want.elem, cx);
        self.set(owner, want, group, By::Reconcile)
    }

    /// The element kind of the current focus's group (§7.4): what OK arms.
    pub fn kind_of<H: Host<Elem = K>>(&self, owner: InputOwner, f: &dyn Focusable<H>, cx: &Cx<'_, H>) -> Option<(FocusKey<K>, ElemKind)> {
        let cur = self.current(owner)?;
        let g = f.group_of(&cur.elem, cx)?;
        let mut groups = Vec::new();
        f.groups(cx, &mut groups);
        let spec = groups.iter().find(|s| s.id == g)?;
        Some((cur, spec.elem))
    }

    /// Take the once-logged fallback flag.
    pub fn take_fell_back(&mut self) -> bool {
        std::mem::take(&mut self.fell_back)
    }

    /// The engine's contribution to the logical-state hash (§7.3 step 5: hashed, restored,
    /// recorded). Keys are written through the app's `IndexElem`-shaped `u32` when they are one;
    /// a host whose elements are not indices supplies its own encoding through `write_with`.
    pub fn write_with(&self, c: &mut Canon, elem: &dyn Fn(&K, &mut Canon)) {
        c.seq(self.scopes.len());
        for (o, k, g) in &self.scopes {
            match o {
                InputOwner::Entry(e) => c.discriminant(0).u32(e.0),
                InputOwner::System(_) => c.discriminant(1),
            };
            c.u32(k.entry.0);
            elem(&k.elem, c);
            c.option(*g, |c, g| {
                c.u32(g.0);
            });
        }
        c.seq(self.remembered.len());
        for ((e, g), k) in &self.remembered {
            c.u32(e.0).u32(g.0);
            elem(k, c);
        }
    }
}

/// The index of `dir` in a `GroupSpec::edge` (`[up, down, left, right]`).
fn side(dir: Dir) -> usize {
    match dir {
        Dir::Up => 0,
        Dir::Down => 1,
        Dir::Left => 2,
        Dir::Right => 3,
    }
}

/// A source placement at a group's head (its top-left corner), for `Seat::First` and a fresh
/// enter with no source.
fn head_of(extent: Rect) -> Placed {
    let r = Rect::new(extent.x, extent.y, 1.0, 1.0);
    Placed {
        rect: r,
        rest_rect: r,
        clip: Rect::FULL,
        index: None,
    }
}

/// The §7.3 step-3 search: the destination group nearest by axis distance weighted 3:1 against
/// orthogonal offset, from the current element's placed rect over every other group's extent,
/// excluding groups whose `reachable` mask omits the axis. A group must lie IN the direction
/// (its extent's far side past the source's centre).
pub fn geometric<'a>(groups: &'a [GroupSpec], cur: GroupId, from: Rect, dir: Dir) -> Option<&'a GroupSpec> {
    let vertical = matches!(dir, Dir::Up | Dir::Down);
    let axis_bit = if vertical { 0b10 } else { 0b01 };
    let (cx, cy) = (from.cx(), from.cy());
    let mut best: Option<(f32, &GroupSpec)> = None;
    for g in groups {
        if g.id == cur || g.len == 0 || g.reachable.0 & axis_bit == 0 {
            continue;
        }
        let e = g.extent;
        // axis distance: from the source's near edge to the group's near edge, in the direction
        let axis = match dir {
            Dir::Down => e.y - (from.y + from.h),
            Dir::Up => from.y - (e.y + e.h),
            Dir::Right => e.x - (from.x + from.w),
            Dir::Left => from.x - (e.x + e.w),
        };
        // in the direction at all: the group's CENTRE is past the source's centre (a group that
        // overlaps the source along the axis — a toolbar spanning the grid's width — is above
        // or below it, never beside it)
        let ahead = match dir {
            Dir::Down => e.cy() > cy,
            Dir::Up => e.cy() < cy,
            Dir::Right => e.cx() > cx,
            Dir::Left => e.cx() < cx,
        };
        if !ahead {
            continue;
        }
        let axis = axis.max(0.0);
        // orthogonal offset: from the source's centre to the group's span on the other axis
        let ortho = if vertical {
            (e.x - cx).max(cx - (e.x + e.w)).max(0.0)
        } else {
            (e.y - cy).max(cy - (e.y + e.h)).max(0.0)
        };
        let score = 3.0 * axis + ortho;
        if best.map_or(true, |(s, _)| score < s) {
            best = Some((score, g));
        }
    }
    best.map(|(_, g)| g)
}

// ---------------------------------------------------------------------------------------------
// the golden tables (§7.7), over pure-layout fixture trees
// ---------------------------------------------------------------------------------------------

#[cfg(test)]
pub(crate) mod tree {
    //! A materialised focus tree — the ONE place one exists, as a test adapter for the golden
    //! tables. Elements are `group * 1000 + index`; every group's elements are rects.
    use super::*;
    use crate::ui::fixture::FixtureHost;
    use crate::ui::machine::Cx;
    use crate::ui::screen::{AxisMask, GroupKind};

    pub struct Group {
        pub spec: GroupSpec,
        pub rects: Vec<Rect>,
        /// `(row, col)` holes for a `Grid`.
        pub holes: Vec<(usize, usize)>,
        /// The group's `SpringTarget` placement differs from the drawn one by this x offset — a
        /// shelf whose scroll spring has a target the drawn scroll has not reached yet.
        pub target_dx: f32,
        /// `reconcile` answers: `(want, answer)` pairs (a Slot→Item promotion, a reorder).
        pub promote: Vec<(u32, u32)>,
    }

    pub struct Tree {
        pub entry: EntryId,
        pub groups: Vec<Group>,
    }

    pub fn key(entry: EntryId, g: u32, i: usize) -> FocusKey<u32> {
        FocusKey {
            entry,
            elem: g * 1000 + i as u32,
        }
    }

    pub fn split(k: u32) -> (u32, usize) {
        (k / 1000, (k % 1000) as usize)
    }

    impl Tree {
        pub fn new(entry: EntryId) -> Self {
            Self {
                entry,
                groups: Vec::new(),
            }
        }

        pub fn group(&mut self, id: u32, kind: GroupKind, seat: Seat, rects: Vec<Rect>) -> &mut Group {
            let extent = rects
                .iter()
                .fold(None::<Rect>, |a, r| Some(a.map_or(*r, |a| a.union(*r))))
                .unwrap_or(Rect::new(0.0, 0.0, 0.0, 0.0));
            self.groups.push(Group {
                spec: GroupSpec {
                    id: GroupId(id),
                    kind,
                    seat,
                    reachable: AxisMask::BOTH,
                    edge: [EdgeRule::Geometric; 4],
                    extent,
                    len: rects.len(),
                    elem: ElemKind::Card,
                },
                rects,
                holes: Vec::new(),
                target_dx: 0.0,
                promote: Vec::new(),
            });
            self.groups.last_mut().unwrap()
        }

        /// A row of `n` tiles of width `w` on pitch `adv`, at `(x0, y)`, height `h`.
        pub fn row(&mut self, id: u32, n: usize, x0: f32, y: f32, w: f32, h: f32, adv: f32) -> &mut Group {
            let rects = (0..n).map(|i| Rect::new(x0 + i as f32 * adv, y, w, h)).collect();
            self.group(id, GroupKind::Row { wrap: false }, Seat::Nearest, rects)
        }

        fn g(&self, id: GroupId) -> Option<&Group> {
            self.groups.iter().find(|g| g.spec.id == id)
        }
    }

    impl Focusable<FixtureHost> for Tree {
        fn groups(&self, _cx: &Cx<'_, FixtureHost>, out: &mut Vec<GroupSpec>) {
            out.extend(self.groups.iter().map(|g| g.spec));
        }
        fn group_of(&self, key: &u32, _cx: &Cx<'_, FixtureHost>) -> Option<GroupId> {
            let (g, i) = split(*key);
            let gr = self.g(GroupId(g))?;
            (i < gr.rects.len()).then_some(gr.spec.id)
        }
        fn neighbour(&self, k: FocusKey<u32>, dir: Dir, _cx: &Cx<'_, FixtureHost>) -> Step<u32> {
            let (gid, i) = split(k.elem);
            let Some(g) = self.g(GroupId(gid)) else {
                return Step::Edge;
            };
            let n = g.rects.len();
            let mv = |j: usize| Step::Move(key(self.entry, gid, j));
            match g.spec.kind {
                GroupKind::Row { wrap } => match dir {
                    Dir::Left if i > 0 => mv(i - 1),
                    Dir::Right if i + 1 < n => mv(i + 1),
                    Dir::Left if wrap && n > 0 => mv(n - 1),
                    Dir::Right if wrap && n > 0 => mv(0),
                    _ => Step::Edge,
                },
                GroupKind::Column => match dir {
                    Dir::Up if i > 0 => mv(i - 1),
                    Dir::Down if i + 1 < n => mv(i + 1),
                    _ => Step::Edge,
                },
                GroupKind::Grid { cols, .. } => {
                    let (r, c) = (i / cols, i % cols);
                    let rows = n.div_ceil(cols);
                    // step along the direction of travel, skipping holes, column memory kept
                    let mut rr = r as i64;
                    let mut cc = c as i64;
                    loop {
                        match dir {
                            Dir::Up => rr -= 1,
                            Dir::Down => rr += 1,
                            Dir::Left => cc -= 1,
                            Dir::Right => cc += 1,
                        }
                        if rr < 0 || cc < 0 || rr as usize >= rows || cc as usize >= cols {
                            return Step::Edge;
                        }
                        let j = rr as usize * cols + cc as usize;
                        if j >= n {
                            return Step::Edge;
                        }
                        if !g.holes.contains(&(rr as usize, cc as usize)) {
                            return mv(j);
                        }
                    }
                }
                GroupKind::Free => {
                    // by geometry among the group's own rects
                    let from = g.rects[i];
                    let mut best: Option<(f32, usize)> = None;
                    for (j, r) in g.rects.iter().enumerate() {
                        if j == i {
                            continue;
                        }
                        let (dx, dy) = (r.cx() - from.cx(), r.cy() - from.cy());
                        let ahead = match dir {
                            Dir::Up => dy < -1.0,
                            Dir::Down => dy > 1.0,
                            Dir::Left => dx < -1.0,
                            Dir::Right => dx > 1.0,
                        };
                        if !ahead {
                            continue;
                        }
                        let (axis, ortho) = if matches!(dir, Dir::Up | Dir::Down) {
                            (dy.abs(), dx.abs())
                        } else {
                            (dx.abs(), dy.abs())
                        };
                        let s = 3.0 * axis + ortho;
                        if best.map_or(true, |(b, _)| s < b) {
                            best = Some((s, j));
                        }
                    }
                    best.map_or(Step::Edge, |(_, j)| mv(j))
                }
                GroupKind::Document => match dir {
                    Dir::Up | Dir::Down if n > 1 => {
                        // a document "scrolls inside" — its one element stays; here the rects are
                        // pages of it and the last is its end
                        let j = if dir == Dir::Down { i + 1 } else { i.wrapping_sub(1) };
                        if j < n {
                            mv(j)
                        } else {
                            Step::Edge
                        }
                    }
                    _ => Step::Edge,
                },
            }
        }
        fn place(&self, k: &u32, _cx: &Cx<'_, FixtureHost>, at: At) -> Option<Placed> {
            let (gid, i) = split(*k);
            let g = self.g(GroupId(gid))?;
            let r = *g.rects.get(i)?;
            let dx = if at == At::SpringTarget { g.target_dx } else { 0.0 };
            let target = Rect::new(r.x + dx, r.y, r.w, r.h);
            Some(Placed {
                rect: target,
                rest_rect: target,
                clip: Rect::FULL,
                index: Some(i as u32),
            })
        }
        fn reconcile(&self, want: FocusKey<u32>, _cx: &Cx<'_, FixtureHost>) -> FocusKey<u32> {
            let (gid, i) = split(want.elem);
            let Some(g) = self.g(GroupId(gid)) else {
                return want;
            };
            if let Some((_, to)) = g.promote.iter().find(|(w, _)| *w == want.elem) {
                return FocusKey {
                    entry: want.entry,
                    elem: *to,
                };
            }
            if i >= g.rects.len() {
                return key(self.entry, gid, g.rects.len().saturating_sub(1));
            }
            want
        }
        fn seat(&self, gid: GroupId, from: Placed, _cx: &Cx<'_, FixtureHost>) -> FocusKey<u32> {
            let Some(g) = self.g(gid) else {
                return key(self.entry, gid.0, 0);
            };
            // `column_near_x`'s contract on a lattice: project the source centre; on a tie keep
            // the index nearest the source's own index when it has one
            let (cx_, cy_) = (from.rect.cx(), from.rect.cy());
            let mut best: Option<(f32, usize)> = None;
            for (j, r) in g.rects.iter().enumerate() {
                let d = match g.spec.kind {
                    GroupKind::Column => (r.cy() - cy_).abs(),
                    _ => (r.cx() - cx_).abs() + (r.cy() - cy_).abs() * 0.001,
                };
                let better = match best {
                    None => true,
                    Some((b, bj)) => {
                        if (d - b).abs() < 1.0e-3 {
                            // a tie: the index nearest the source's own (`column_near_x`'s
                            // `from`), else the lower index
                            match from.index {
                                Some(src) => (j as i64 - src as i64).abs() < (bj as i64 - src as i64).abs(),
                                None => false,
                            }
                        } else {
                            d < b
                        }
                    }
                };
                if better {
                    best = Some((d, j));
                }
            }
            key(self.entry, gid.0, best.map_or(0, |b| b.1))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::tree::{key, split, Tree};
    use super::*;
    use crate::ui::fixture::{FixtureHost, FixtureMeasure, FixtureView, FixtureViews};
    use crate::ui::machine::{Cx, FocusRead, PressRead, Tick};
    use crate::ui::screen::{AxisMask, GroupKind};

    const E: EntryId = EntryId(1);
    const OWNER: InputOwner = InputOwner::Entry(E);

    fn cx<'a>(m: &'a FixtureMeasure, v: &'a FixtureView) -> Cx<'a, FixtureHost> {
        Cx {
            views: FixtureViews { store: v },
            tick: Tick::default(),
            measure: m,
            press: PressRead::default(),
            focus: FocusRead::default(),
            owner: OWNER,
        }
    }

    macro_rules! rig {
        ($m:ident, $v:ident, $cx:ident) => {
            let $m = FixtureMeasure;
            let $v = FixtureView::default();
            let $cx = cx(&$m, &$v);
        };
    }

    /// The fixture trees of §7.7.
    fn trees() -> Vec<(&'static str, Tree)> {
        let mut out = Vec::new();
        // a grid
        let mut t = Tree::new(E);
        let rects: Vec<Rect> = (0..9).map(|i| Rect::new(100.0 + (i % 3) as f32 * 200.0, 100.0 + (i / 3) as f32 * 200.0, 180.0, 180.0)).collect();
        t.group(1, GroupKind::Grid { cols: 3, holes: &[] }, Seat::Nearest, rects);
        out.push(("grid", t));
        // a table
        let mut t = Tree::new(E);
        let rects: Vec<Rect> = (0..5).map(|i| Rect::new(600.0, 100.0 + i as f32 * 80.0, 700.0, 70.0)).collect();
        t.group(2, GroupKind::Column, Seat::Remembered, rects);
        out.push(("table", t));
        // a strip over a row
        let mut t = Tree::new(E);
        t.row(3, 4, 100.0, 20.0, 160.0, 48.0, 180.0);
        t.row(4, 6, 60.0, 300.0, 220.0, 330.0, 240.0);
        out.push(("strip-over-row", t));
        // a row above a wider row, and two rows of different tile widths
        let mut t = Tree::new(E);
        t.row(5, 3, 300.0, 100.0, 200.0, 120.0, 220.0);
        t.row(6, 8, 60.0, 400.0, 220.0, 330.0, 240.0);
        t.row(7, 5, 60.0, 800.0, 360.0, 200.0, 380.0);
        out.push(("rows", t));
        // a grid with a hole (the PIN pad)
        let mut t = Tree::new(E);
        let rects: Vec<Rect> = (0..12).map(|i| Rect::new(700.0 + (i % 3) as f32 * 120.0, 200.0 + (i / 3) as f32 * 120.0, 100.0, 100.0)).collect();
        let g = t.group(8, GroupKind::Grid { cols: 3, holes: &[] }, Seat::Nearest, rects);
        g.holes = vec![(3, 0)];
        out.push(("pin-pad", t));
        // a virtualised group whose focused element is off-viewport
        let mut t = Tree::new(E);
        t.row(9, 40, 60.0, 300.0, 220.0, 330.0, 240.0);
        t.row(10, 3, 60.0, 700.0, 220.0, 330.0, 240.0);
        out.push(("virtualised", t));
        out
    }

    /// `every_stop_times_every_direction_on_the_fixture_trees`: no panic, and every answer is
    /// either a key the tree places or an edge rule — from every element, in every direction.
    #[test]
    fn every_stop_times_every_direction_on_the_fixture_trees() {
        rig!(m, v, cx);
        for (name, t) in trees() {
            for g in &t.groups {
                for i in 0..g.rects.len() {
                    for dir in [Dir::Up, Dir::Down, Dir::Left, Dir::Right] {
                        let mut e = FocusEngine::new();
                        let k = key(E, g.spec.id.0, i);
                        e.set(OWNER, k, Some(g.spec.id), By::Restore);
                        match e.move_dir(OWNER, &t, &[], dir, &cx) {
                            Outcome::Moved { to, .. } => {
                                assert!(t.place(&to.elem, &cx, At::Drawn).is_some(), "{name}: {dir:?} from {} landed on nothing", k.elem);
                                assert_eq!(e.current(OWNER), Some(to));
                            }
                            Outcome::Edge(_) | Outcome::Nothing => {}
                        }
                    }
                }
            }
        }
    }

    /// `card_row::column_near_x`'s contract, relocated onto the engine: DOWN from a row lands on
    /// the tile under the cursor's centre in the wider row beneath.
    #[test]
    fn down_from_a_row_lands_nearest_by_the_column_near_x_contract() {
        rig!(m, v, cx);
        let (_, t) = trees().into_iter().find(|(n, _)| *n == "rows").unwrap();
        let mut e = FocusEngine::new();
        // row 5: tiles at x=300,520,740 (w 200); row 6: pitch 240 from x=60 → centres 170,410,650,890…
        e.set(OWNER, key(E, 5, 2), Some(GroupId(5)), By::Restore); // centre x = 840
        let out = e.move_dir(OWNER, &t, &[], Dir::Down, &cx);
        let Outcome::Moved { to, by, .. } = out else {
            panic!("{out:?}");
        };
        assert_eq!(by, By::Dir);
        assert_eq!(split(to.elem), (6, 3), "centre 840 → the tile centred at 890 (index 3)");
    }

    /// The tie-break: bouncing UP/DOWN between two rows offset by half a tile never walks focus
    /// sideways (`[7, 8, 8, 9, 9, 10, 10, 11]` was the failure on a plain round-to-nearest).
    #[test]
    fn bouncing_between_two_rows_is_stable() {
        rig!(m, v, cx);
        let mut t = Tree::new(E);
        t.row(1, 12, 0.0, 100.0, 200.0, 100.0, 200.0);
        t.row(2, 12, 100.0, 300.0, 200.0, 100.0, 200.0); // offset by exactly half a tile
        let mut e = FocusEngine::new();
        e.set(OWNER, key(E, 1, 7), Some(GroupId(1)), By::Restore);
        let mut seen = Vec::new();
        for i in 0..8 {
            let dir = if i % 2 == 0 { Dir::Down } else { Dir::Up };
            let Outcome::Moved { to, .. } = e.move_dir(OWNER, &t, &[], dir, &cx) else {
                panic!("a move");
            };
            seen.push(split(to.elem).1);
        }
        // every return to row 1 is index 7; every landing in row 2 is the same index
        assert!(seen.iter().step_by(2).all(|i| *i == seen[0]), "{seen:?}");
        assert!(seen.iter().skip(1).step_by(2).all(|i| *i == 7), "{seen:?}");
    }

    /// `Seat::Remembered` is UNCONDITIONAL: the table's remembered row is re-entered even when
    /// it is far off the viewport (the owner then reveals it — a different path from shelf to
    /// shelf, by construction).
    #[test]
    fn remembered_entry_reveals_an_off_screen_element() {
        rig!(m, v, cx);
        let mut t = Tree::new(E);
        t.row(1, 3, 100.0, 20.0, 160.0, 48.0, 180.0);
        let rects: Vec<Rect> = (0..40).map(|i| Rect::new(600.0, 100.0 + i as f32 * 80.0, 700.0, 70.0)).collect();
        t.group(2, GroupKind::Column, Seat::Remembered, rects);
        let mut e = FocusEngine::new();
        e.set(OWNER, key(E, 2, 35), Some(GroupId(2)), By::Restore); // deep in the table
        e.set(OWNER, key(E, 1, 0), Some(GroupId(1)), By::Restore); // up to the strip
        let Outcome::Moved { to, .. } = e.move_dir(OWNER, &t, &[], Dir::Down, &cx) else {
            panic!("a move");
        };
        assert_eq!(split(to.elem), (2, 35), "the remembered row, however far");
    }

    /// `RememberedNear{rows}` chases the cursor only within `rows`; beyond that it is Nearest.
    #[test]
    fn remembered_near_falls_to_nearest_beyond_its_rows() {
        rig!(m, v, cx);
        let mut t = Tree::new(E);
        t.row(1, 6, 60.0, 100.0, 220.0, 100.0, 240.0);
        let g = t.row(2, 6, 60.0, 400.0, 220.0, 100.0, 240.0);
        g.spec.seat = Seat::RememberedNear { rows: 1 };
        let mut e = FocusEngine::new();
        e.set(OWNER, key(E, 2, 5), Some(GroupId(2)), By::Restore); // remembered at the far right
        e.set(OWNER, key(E, 1, 0), Some(GroupId(1)), By::Restore); // cursor at the far left
        let Outcome::Moved { to, .. } = e.move_dir(OWNER, &t, &[], Dir::Down, &cx) else {
            panic!("a move");
        };
        assert_eq!(split(to.elem), (2, 0), "five tiles away: nearest, not remembered");
        // that landing is now the group's remembered cursor; put it back at the far right
        e.set(OWNER, key(E, 2, 5), Some(GroupId(2)), By::Restore);
        e.set(OWNER, key(E, 1, 4), Some(GroupId(1)), By::Restore); // one tile from the remembered
        let Outcome::Moved { to, .. } = e.move_dir(OWNER, &t, &[], Dir::Down, &cx) else {
            panic!("a move");
        };
        assert_eq!(split(to.elem), (2, 5), "within one tile: the remembered one");
    }

    /// `Seat::Projected`: the rail→grid door goes through the container's `seat`.
    #[test]
    fn a_seated_door_projects_through_the_container() {
        rig!(m, v, cx);
        let mut t = Tree::new(E);
        let rail: Vec<Rect> = (0..5).map(|i| Rect::new(20.0, 100.0 + i as f32 * 60.0, 40.0, 50.0)).collect();
        t.group(1, GroupKind::Column, Seat::Nearest, rail);
        let grid: Vec<Rect> = (0..6).map(|i| Rect::new(200.0 + (i % 3) as f32 * 200.0, 100.0 + (i / 3) as f32 * 200.0, 180.0, 180.0)).collect();
        let g = t.group(2, GroupKind::Grid { cols: 3, holes: &[] }, Seat::Projected, grid);
        g.spec.reachable = AxisMask::BOTH;
        let mut e = FocusEngine::new();
        e.set(OWNER, key(E, 1, 4), Some(GroupId(1)), By::Restore); // rail letter 4, y≈365
        let Outcome::Moved { to, .. } = e.move_dir(OWNER, &t, &[], Dir::Right, &cx) else {
            panic!("a move");
        };
        // the projection: the grid row nearest y 365 is row 1 (y 300..480), column 0
        assert_eq!(split(to.elem), (2, 3));
    }

    /// A hole is skipped along the direction of travel with column memory preserved.
    #[test]
    fn a_grid_hole_is_skipped_along_the_direction_of_travel() {
        rig!(m, v, cx);
        let (_, t) = trees().into_iter().find(|(n, _)| *n == "pin-pad").unwrap();
        let mut e = FocusEngine::new();
        e.set(OWNER, key(E, 8, 6), Some(GroupId(8)), By::Restore); // row 2, col 0 (the '7')
        let out = e.move_dir(OWNER, &t, &[], Dir::Down, &cx);
        // (3,0) is the hole: nothing below in that column → an edge
        assert!(matches!(out, Outcome::Nothing | Outcome::Edge(_)), "{out:?}");
        e.set(OWNER, key(E, 8, 7), Some(GroupId(8)), By::Restore); // row 2, col 1
        let Outcome::Moved { to, .. } = e.move_dir(OWNER, &t, &[], Dir::Down, &cx) else {
            panic!("a move");
        };
        assert_eq!(split(to.elem).1, 10, "the '0' under the '8'");
    }

    /// A document scrolls inside and leaves only at its ends (the engine-level half of
    /// `geom`'s test).
    #[test]
    fn a_document_scrolls_inside_and_leaves_at_its_ends() {
        rig!(m, v, cx);
        let mut t = Tree::new(E);
        t.row(1, 2, 100.0, 20.0, 160.0, 48.0, 180.0);
        let pages: Vec<Rect> = (0..3).map(|i| Rect::new(400.0, 200.0 + i as f32 * 300.0, 1000.0, 280.0)).collect();
        t.group(2, GroupKind::Document, Seat::First, pages);
        let mut e = FocusEngine::new();
        e.set(OWNER, key(E, 2, 0), Some(GroupId(2)), By::Restore);
        let Outcome::Moved { to, .. } = e.move_dir(OWNER, &t, &[], Dir::Down, &cx) else {
            panic!("inside");
        };
        assert_eq!(split(to.elem), (2, 1));
        e.move_dir(OWNER, &t, &[], Dir::Down, &cx);
        let out = e.move_dir(OWNER, &t, &[], Dir::Down, &cx);
        assert!(matches!(out, Outcome::Nothing), "at the end nothing lies below: {out:?}");
        let Outcome::Moved { to, .. } = e.move_dir(OWNER, &t, &[], Dir::Up, &cx) else {
            panic!("back inside");
        };
        assert_eq!(split(to.elem), (2, 1));
        e.move_dir(OWNER, &t, &[], Dir::Up, &cx);
        let Outcome::Moved { to, .. } = e.move_dir(OWNER, &t, &[], Dir::Up, &cx) else {
            panic!("leaves at the top");
        };
        assert_eq!(split(to.elem).0, 1, "…to the strip above");
    }

    /// A `Link` replaces the geometric answer for ITS direction only.
    #[test]
    fn a_link_replaces_the_geometric_answer_for_its_direction_only() {
        rig!(m, v, cx);
        let mut t = Tree::new(E);
        t.row(1, 3, 100.0, 20.0, 160.0, 48.0, 180.0); // the strip
        t.row(2, 3, 100.0, 200.0, 160.0, 48.0, 180.0); // a hero action row, geometrically nearest
        t.row(3, 6, 60.0, 600.0, 220.0, 330.0, 240.0); // the grid, far below
        let link = Link {
            from: GroupId(1),
            dir: Dir::Down,
            to: GroupId(3),
        };
        let mut e = FocusEngine::new();
        e.set(OWNER, key(E, 1, 1), Some(GroupId(1)), By::Restore);
        let Outcome::Moved { to, .. } = e.move_dir(OWNER, &t, &[link], Dir::Down, &cx) else {
            panic!("a move");
        };
        assert_eq!(split(to.elem).0, 3, "the link's destination, not the nearest group");
        // the other directions are untouched: UP from the strip has nothing above
        e.set(OWNER, key(E, 1, 1), Some(GroupId(1)), By::Restore);
        assert!(matches!(e.move_dir(OWNER, &t, &[link], Dir::Up, &cx), Outcome::Nothing));
        // and without the link, geometry answers the hero row
        e.set(OWNER, key(E, 1, 1), Some(GroupId(1)), By::Restore);
        let Outcome::Moved { to, .. } = e.move_dir(OWNER, &t, &[], Dir::Down, &cx) else {
            panic!("a move");
        };
        assert_eq!(split(to.elem).0, 2);
    }

    /// A group whose `reachable` mask omits the axis is never a destination along it (the
    /// Library rail is horizontal-only).
    #[test]
    fn an_unreachable_axis_group_is_never_a_destination() {
        rig!(m, v, cx);
        let mut t = Tree::new(E);
        let rail: Vec<Rect> = (0..5).map(|i| Rect::new(20.0, 100.0 + i as f32 * 60.0, 40.0, 50.0)).collect();
        let g = t.group(1, GroupKind::Column, Seat::Nearest, rail);
        g.spec.reachable = AxisMask::HORIZONTAL;
        t.row(2, 3, 100.0, 20.0, 160.0, 48.0, 180.0); // a toolbar above the grid
        t.row(3, 6, 200.0, 500.0, 180.0, 180.0, 200.0);
        let mut e = FocusEngine::new();
        // UP from the grid's first tile: the rail is nearer by geometry, but vertically unreachable
        e.set(OWNER, key(E, 3, 0), Some(GroupId(3)), By::Restore);
        let Outcome::Moved { to, .. } = e.move_dir(OWNER, &t, &[], Dir::Up, &cx) else {
            panic!("a move");
        };
        assert_eq!(split(to.elem).0, 2, "the toolbar, never the rail");
        // LEFT from the grid does reach the rail
        e.set(OWNER, key(E, 3, 0), Some(GroupId(3)), By::Restore);
        let Outcome::Moved { to, .. } = e.move_dir(OWNER, &t, &[], Dir::Left, &cx) else {
            panic!("a move");
        };
        assert_eq!(split(to.elem).0, 1);
    }

    /// A store landing: `reconcile` promotes a Slot to the Item that now fills it, and a reorder
    /// keeps the film's key — both as explicit `Reconcile` moves.
    #[test]
    fn slot_to_item_promotion_is_an_explicit_reconcile() {
        rig!(m, v, cx);
        let mut t = Tree::new(E);
        let g = t.row(1, 6, 60.0, 300.0, 220.0, 330.0, 240.0);
        // slot 2 is filled by the item keyed 1000+2 → the owner promotes it to element 5 (a
        // different key: the item's), and the reorder moves item 4 to position 0
        g.promote = vec![(1002, 1005), (1004, 1000)];
        let mut e = FocusEngine::new();
        e.set(OWNER, key(E, 1, 2), Some(GroupId(1)), By::Restore);
        let Outcome::Moved { from, to, by } = e.reconcile(OWNER, &t, &cx) else {
            panic!("a reconcile move");
        };
        assert_eq!(by, By::Reconcile);
        assert_eq!(from.map(|k| k.elem), Some(1002));
        assert_eq!(to.elem, 1005);
        assert!(matches!(e.reconcile(OWNER, &t, &cx), Outcome::Nothing), "settled");
    }

    /// `the_same_film_keeps_its_key_after_a_shelf_reorder`: the reorder path of `reconcile`.
    #[test]
    fn the_same_film_keeps_its_key_after_a_shelf_reorder() {
        rig!(m, v, cx);
        let mut t = Tree::new(E);
        let g = t.row(1, 6, 60.0, 300.0, 220.0, 330.0, 240.0);
        g.promote = vec![(1004, 1000)];
        let mut e = FocusEngine::new();
        e.set(OWNER, key(E, 1, 4), Some(GroupId(1)), By::Restore);
        let Outcome::Moved { to, by, .. } = e.reconcile(OWNER, &t, &cx) else {
            panic!("a reconcile move");
        };
        assert_eq!((to.elem, by), (1000, By::Reconcile), "focus followed the item to its new position");
    }

    /// Movement resolves against the SPRING TARGET, not the drawn rect: a shelf whose scroll
    /// spring is still travelling places its tiles where they are GOING, so two fast presses
    /// land where focus will be, not where the last frame drew it.
    #[test]
    fn two_fast_presses_resolve_against_the_spring_target() {
        rig!(m, v, cx);
        let mut t = Tree::new(E);
        t.row(1, 6, 0.0, 0.0, 100.0, 40.0, 100.0);
        t.row(2, 3, 0.0, 200.0, 150.0, 150.0, 160.0);
        let mut e = FocusEngine::new();
        // drawn == target: tile 1 below (cx 235) is under tile 2 above
        e.set(OWNER, key(E, 2, 1), Some(GroupId(2)), By::Restore);
        let Outcome::Moved { to, .. } = e.move_dir(OWNER, &t, &[], Dir::Up, &cx) else {
            panic!("a move");
        };
        assert_eq!(split(to.elem).1, 2);
        // the shelf's scroll target is 120 px further along: the tile's TARGET centre is 355,
        // under tile 3 above, while its drawn centre is still 235
        t.groups[1].target_dx = 120.0;
        let drawn = t.place(&key(E, 2, 1).elem, &cx, At::Drawn).unwrap();
        let target = t.place(&key(E, 2, 1).elem, &cx, At::SpringTarget).unwrap();
        assert_ne!(drawn.rect.cx(), target.rect.cx());
        e.set(OWNER, key(E, 2, 1), Some(GroupId(2)), By::Restore);
        let Outcome::Moved { to, .. } = e.move_dir(OWNER, &t, &[], Dir::Up, &cx) else {
            panic!("a move");
        };
        assert_eq!(split(to.elem).1, 3, "resolved against the target, not the drawn rect");
    }

    /// The geometric search itself, as a table: axis distance outweighs orthogonal offset 3:1.
    #[test]
    fn the_geometric_search_weights_axis_distance_three_to_one() {
        let mk = |id: u32, r: Rect| GroupSpec {
            id: GroupId(id),
            kind: GroupKind::Row { wrap: false },
            seat: Seat::Nearest,
            reachable: AxisMask::BOTH,
            edge: [EdgeRule::Geometric; 4],
            extent: r,
            len: 1,
            elem: ElemKind::Card,
        };
        let from = Rect::new(0.0, 0.0, 100.0, 100.0);
        // A: 100 px below, 250 px to the side (score 3·100 + 250 = 550); B: 200 px below, aligned (600)
        let a = mk(1, Rect::new(350.0, 200.0, 100.0, 100.0));
        let b = mk(2, Rect::new(0.0, 300.0, 100.0, 100.0));
        let groups = [mk(0, from), a, b];
        assert_eq!(geometric(&groups, GroupId(0), from, Dir::Down).map(|g| g.id), Some(GroupId(1)));
        // …and a group ABOVE is never a Down destination
        let up = mk(3, Rect::new(0.0, -300.0, 100.0, 100.0));
        let groups = [mk(0, from), up];
        assert!(geometric(&groups, GroupId(0), from, Dir::Down).is_none());
        assert_eq!(geometric(&groups, GroupId(0), from, Dir::Up).map(|g| g.id), Some(GroupId(3)));
    }
}
