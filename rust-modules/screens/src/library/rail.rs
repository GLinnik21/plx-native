//! Fixed-band rail owned by MasterDetail. Only Input owns its focused/remembered letter.
use std::ffi::CString;
use super::identity::{KeyRegistry, KeyRegion, region_of_elem};
use super::layout::{rail_geom, rail_scroll_target, RAIL_CAP_PAD, RAIL_PITCH, RAIL_TRACK_W};
use crate::registry::{LibraryIdentity, LibraryLike, LibrarySectionIdentity};
use plx_ui::consts::K_SCROLL;
use plx_ui::frame::Budget;
use plx_machine::machine::{Canon, Cx, EntryId, FocusKey, GroupId};
use plx_ui::screen::{Activate, At, AxisMask, Dir, DrawFrame, EdgeRule, ElemKind, Focusable,
    GroupKind, GroupSpec, Hover, Part, Placed, Seat, Step, Stop};
use plx_ui::{Rect, Spring, theme};

/// The focus disc behind a letter: two pixels inside the letter pitch.
const FOCUS_DISC: f32 = RAIL_PITCH - 2.0;
/// A letter with no titles shows at this fraction of its ink and focus steps over it.
pub(super) const EMPTY_LETTER_ALPHA: f32 = 0.25;
/// The scroll speed (px/s of the page's scroll spring) above which the rail shows itself: a
/// held D-pad crossing several rows, not a single row step.
pub(super) const FAST_SCROLL_VEL: f32 = 3000.0;

/// What the page tells the rail each frame so it can decide whether to be on screen: the rail is
/// hidden at rest and appears only on a fast scroll or when focus reaches the right edge.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct RailHint {
    /// The page's scroll spring velocity.
    pub scroll_vel: f32,
    /// Focus is on the grid's last column (one Right press from the rail).
    pub right_edge: bool,
}

impl RailHint {
    /// Whether the rail should be shown for this hint; `in_rail` is focus already on a letter,
    /// which always keeps it up.
    pub(super) fn wants_rail(self, in_rail: bool) -> bool {
        in_rail || self.right_edge || self.scroll_vel.abs() > FAST_SCROLL_VEL
    }
}

pub(super) struct RailPart {
    entry: EntryId,
    group: GroupId,
    pub(super) elems: Vec<u32>,
    labels: Vec<CString>,
    starts: Vec<usize>,
    rect: Rect,
    scroll: Spring,
    scroll_target: f32,
    alpha: Spring,
}

impl RailPart {
    pub(super) const SHAPE: &'static str = "LibraryRail{group:u32,elems:[u32],labels:[str],starts:[u32],rect:{x:f32,y:f32,w:f32,h:f32},scroll:Spring{pos:f32,vel:f32},scroll_target:f32,alpha:Spring{pos:f32,vel:f32}}";

    pub(super) fn new(entry: EntryId, group: GroupId) -> Self {
        Self { entry, group, elems: Vec::new(), labels: Vec::new(), starts: Vec::new(),
            rect: Rect::new(0.0, 0.0, 0.0, 0.0), scroll: Spring::at(0.0),
            scroll_target: 0.0, alpha: Spring::at(0.0) }
    }

    pub(super) fn clear_projection(&mut self) {
        self.elems.clear(); self.labels.clear(); self.starts.clear();
        self.scroll.jump(0.0); self.scroll_target = 0.0;
    }

    pub(super) fn write(&self, c: &mut Canon) {
        let Self { entry: _, group, elems, labels, starts, rect, scroll, scroll_target, alpha } = self;
        c.u32(group.0).seq(elems.len());
        for elem in elems { c.u32(*elem); }
        c.seq(labels.len());
        for label in labels { c.str(label.to_str().unwrap_or("")); }
        c.seq(starts.len());
        for index in starts { c.u32(*index as u32); }
        c.f32(rect.x).f32(rect.y).f32(rect.w).f32(rect.h);
        c.f32(scroll.pos).f32(scroll.vel).f32(*scroll_target).f32(alpha.pos).f32(alpha.vel);
    }

    pub(super) fn refresh<H: LibraryLike>(&mut self, cx: &Cx<'_, H>, keys: &mut KeyRegistry) {
        let view = H::listing(cx);
        let Some(id) = view.id() else { self.clear_projection(); return };
        self.elems.clear(); self.labels.clear(); self.starts.clear();
        if view.total() <= 0 || !view.rail_available() { self.clear_projection(); return; }
        let section = LibrarySectionIdentity { sid: id.sid, key: id.section };
        for (index, (label, _)) in view.letters().iter().enumerate() {
            self.starts.push(view.letter_start(index));
            self.elems.push(keys.register(LibraryIdentity::Rail { section: section.clone(), label: label.clone() }, self.group, index));
            self.labels.push(CString::new(label.as_str()).unwrap_or_default());
        }
        let (y, cx, height, max) = rail_geom(self.elems.len());
        self.rect = Rect::new(cx - RAIL_TRACK_W * 0.5, y, RAIL_TRACK_W, height);
        self.scroll_target = self.scroll_target.clamp(0.0, max);
    }

    fn in_region<H: LibraryLike>(&self, cx: &Cx<'_, H>) -> bool {
        cx.focus.current.filter(|key| key.entry == self.entry).is_some_and(|key|
            matches!(region_of_elem(key.elem), Some(KeyRegion::Grid | KeyRegion::Rail)))
    }

    pub(super) fn eligible<H: LibraryLike>(&self, cx: &Cx<'_, H>) -> bool {
        !self.elems.is_empty() && self.in_region(cx)
    }

    /// Grid index is a current-only projection passed from the same engine key, never retained.
    pub(super) fn advance<H: LibraryLike>(&mut self, cx: &Cx<'_, H>, grid_index: Option<usize>, hint: RailHint, live: bool, dt: f32) {
        let drive = cx.focus.current.filter(|key| key.entry == self.entry)
            .and_then(|key| self.elems.iter().position(|elem| *elem == key.elem))
            .or_else(|| grid_index.map(|index| self.letter_for_item(index)));
        let in_rail = cx.focus.current.filter(|key| key.entry == self.entry)
            .is_some_and(|key| region_of_elem(key.elem) == Some(KeyRegion::Rail));
        let active = live && !self.elems.is_empty() && self.in_region(cx) && hint.wants_rail(in_rail);
        self.step_motion(drive, active, dt);
    }

    fn step_motion(&mut self, drive: Option<usize>, active: bool, dt: f32) {
        if let Some(drive) = drive {
            self.scroll_target = rail_scroll_target(self.scroll.pos, drive, self.elems.len());
        }
        self.scroll.step(self.scroll_target, K_SCROLL, dt);
        self.alpha.step(if active { 1.0 } else { 0.0 }, K_SCROLL, dt);
    }

    /// A letter no title starts with: the next letter begins at the same item. (The last letter
    /// cannot be told from here without the total, which the rail does not keep; a listing only
    /// ends on a letter that has titles.)
    fn letter_empty(&self, index: usize) -> bool {
        match (self.starts.get(index), self.starts.get(index + 1)) {
            (Some(a), Some(b)) => a == b,
            _ => false,
        }
    }

    /// The nearest letter with titles at or before `index`, else the first one after it.
    fn skip_empty(&self, index: usize) -> usize {
        (0..=index.min(self.elems.len().saturating_sub(1))).rev().find(|i| !self.letter_empty(*i))
            .or_else(|| (index..self.elems.len()).find(|i| !self.letter_empty(*i)))
            .unwrap_or(index)
    }

    pub(super) fn letter_for_item(&self, index: usize) -> usize {
        self.starts.iter().enumerate().rev().find(|(_, start)| **start <= index).map(|(i, _)| i).unwrap_or(0)
    }

    pub(super) fn start_for_elem(&self, elem: u32) -> Option<usize> {
        self.elems.iter().position(|key| *key == elem).and_then(|index| self.starts.get(index).copied())
    }

    fn slot(&self, index: usize, at: At) -> Rect {
        let scroll = match at {
            At::Drawn => self.scroll.pos,
            At::SpringTarget => rail_scroll_target(self.scroll.pos, index, self.elems.len()),
        };
        Rect::new(self.rect.x, self.rect.y + index as f32 * RAIL_PITCH - scroll, RAIL_TRACK_W, RAIL_PITCH)
    }

    fn letter_alpha(&self, index: usize) -> f32 {
        let rel = self.slot(index, At::Drawn).cy() - self.rect.y;
        let max = rail_geom(self.elems.len()).3;
        let mut alpha = 1.0f32;
        if self.scroll.pos > 0.5 { alpha = alpha.min(rel / RAIL_PITCH); }
        if self.scroll.pos < max - 0.5 { alpha = alpha.min((self.rect.h - rel) / RAIL_PITCH); }
        alpha.clamp(0.0, 1.0)
    }

    pub(super) fn record_stops<H: LibraryLike>(&self, f: &mut DrawFrame<'_, '_, H>) {
        if !f.records_stops() { return; }
        if !self.in_region(f.cx) { return; }
        for (index, &elem) in self.elems.iter().enumerate() {
            if self.letter_alpha(index) <= 0.5 || self.letter_empty(index) { continue; }
            let Some(placed) = <Self as Focusable<H>>::place(self, &elem, f.cx, At::Drawn) else { continue };
            f.stop(f.painter, Stop { key: FocusKey { entry: self.entry, elem },
                rect: placed.rect, rest_rect: placed.rest_rect, clip: placed.clip,
                hover: Hover::Focus, activate: Activate::Direct });
        }
    }

    pub(super) fn draw_with_current<H: LibraryLike>(&self, f: &mut DrawFrame<'_, '_, H>, _frame: Rect, grid_index: Option<usize>) {
        if self.elems.is_empty() || (!self.in_region(f.cx) && self.alpha.pos <= 0.01) { return; }
        let p = f.painter.alpha(f.page_alpha * self.alpha.pos);
        let track = Rect::new(self.rect.x, self.rect.y - RAIL_CAP_PAD, self.rect.w, self.rect.h + 2.0 * RAIL_CAP_PAD);
        p.rect_sheened(track, RAIL_TRACK_W * 0.5, theme::scrim_black(0.30), theme::scrim_black(0.40));
        let _clip = (rail_geom(self.elems.len()).3 > 0.5).then(|| f.clip(p, self.rect));
        let current = grid_index.map(|index| self.letter_for_item(index));
        for (index, label) in self.labels.iter().enumerate() {
            let alpha = self.letter_alpha(index)
                * if self.letter_empty(index) { EMPTY_LETTER_ALPHA } else { 1.0 };
            if alpha <= 0.0 { continue; }
            let q = p.alpha(alpha);
            let slot = self.slot(index, At::Drawn);
            let focused = f.focus.current == Some(FocusKey { entry: self.entry, elem: self.elems[index] });
            let ink = if focused {
                let disc = Rect::new(slot.cx() - FOCUS_DISC * 0.5, slot.cy() - FOCUS_DISC * 0.5, FOCUS_DISC, FOCUS_DISC);
                q.rect(disc, FOCUS_DISC * 0.5, plx_ui::ACCENT, plx_ui::ACCENT, 0.0);
                plx_ui::ACCENT_INK
            } else if current == Some(index) { theme::TEXT_PRIMARY } else { theme::TEXT_SECONDARY };
            plx_ui::label::Label::new(label.as_ptr(), theme::size::MICRO, ink).bold()
                .h(plx_ui::label::HAlign::Center).draw(q, slot);
        }
        self.record_stops(f);
    }
}

impl<H: LibraryLike> Focusable<H> for RailPart {
    fn groups(&self, cx: &Cx<'_, H>, out: &mut Vec<GroupSpec>) {
        if self.elems.is_empty() || !self.in_region(cx) { return; }
        out.push(GroupSpec { id: self.group, kind: GroupKind::Column, seat: Seat::First,
            reachable: AxisMask::HORIZONTAL,
            edge: [EdgeRule::Stop, EdgeRule::Stop, EdgeRule::Geometric, EdgeRule::Stop],
            extent: self.rect, len: self.elems.len(), elem: ElemKind::Bare });
    }
    fn group_of(&self, elem: &u32, cx: &Cx<'_, H>) -> Option<GroupId> {
        (self.in_region(cx) && self.elems.contains(elem)).then_some(self.group)
    }
    fn neighbour(&self, key: FocusKey<u32>, dir: Dir, _: &Cx<'_, H>) -> Step<u32> {
        let Some(index) = self.elems.iter().position(|elem| *elem == key.elem) else { return Step::Edge };
        let next = match dir {
            Dir::Up => (0..index).rev().find(|i| !self.letter_empty(*i)),
            Dir::Down => (index + 1..self.elems.len()).find(|i| !self.letter_empty(*i)),
            _ => None,
        };
        next.map_or(Step::Edge, |i| Step::Move(FocusKey { entry: self.entry, elem: self.elems[i] }))
    }
    fn place(&self, elem: &u32, cx: &Cx<'_, H>, at: At) -> Option<Placed> {
        if !self.in_region(cx) { return None; }
        let index = self.elems.iter().position(|key| key == elem)?;
        let rect = self.slot(index, at);
        Some(Placed { rect, rest_rect: rect, clip: self.rect, index: Some(index as u32) })
    }
    fn reconcile(&self, want: FocusKey<u32>, _: &Cx<'_, H>) -> FocusKey<u32> {
        if self.elems.contains(&want.elem) { want } else {
            self.elems.first().map(|elem| FocusKey { entry: self.entry, elem: *elem }).unwrap_or(want)
        }
    }
    fn seat(&self, _: GroupId, from: Placed, _: &Cx<'_, H>) -> FocusKey<u32> {
        let index = self.skip_empty(self.letter_for_item(from.index.unwrap_or(0) as usize));
        FocusKey { entry: self.entry, elem: self.elems.get(index).copied().unwrap_or(0) }
    }
}
impl<H: LibraryLike> Part<H> for RailPart {
    fn prepare(&mut self, _: &mut Budget, _: &Cx<'_, H>) {}
    fn draw(&mut self, f: &mut DrawFrame<'_, '_, H>, frame: Rect) { self.draw_with_current(f, frame, None); }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model() -> RailPart {
        let mut rail = RailPart::new(EntryId(1), GroupId(2));
        rail.elems = (0..30).collect();
        let (y, x, h, _) = rail_geom(30);
        rail.rect = Rect::new(x - RAIL_TRACK_W * 0.5, y, RAIL_TRACK_W, h);
        rail
    }

    #[test]
    fn rail_scroll_and_presence_animate_then_settle_outside_the_grid_region() {
        let _guard = plx_base::testlock::serial();
        let mut rail = model();
        let (_, moving) = plx_machine::idle::scoped_motion(|| rail.step_motion(Some(29), true, 0.016));
        assert!(moving, "entering the region reports its scroll and fade motion");
        assert!(rail.scroll.pos > 0.0 && rail.scroll.pos < rail.scroll_target);
        assert!(rail.alpha.pos > 0.0 && rail.alpha.pos < 1.0);
        for _ in 0..400 { rail.step_motion(Some(29), true, 0.016); }
        assert!((rail.scroll.pos - rail_geom(30).3).abs() < 0.001);
        assert!((rail.alpha.pos - 1.0).abs() < 0.001 && rail.alpha.vel.abs() < 0.001);
        for _ in 0..400 { rail.step_motion(None, false, 0.016); }
        assert!(rail.alpha.pos.abs() < 0.001 && rail.alpha.vel.abs() < 0.001);
        let (_, moving) = plx_machine::idle::scoped_motion(|| rail.step_motion(None, false, 0.016));
        assert!(!moving, "a resting hidden rail requests no further animated presents");
    }

    #[test]
    fn quick_letter_steps_place_against_settled_scroll_and_keep_the_fade_margin() {
        let _guard = plx_base::testlock::serial();
        let mut rail = model();
        rail.step_motion(Some(27), true, 0.016);
        for drive in [28, 29] {
            let target = rail.slot(drive, At::SpringTarget);
            let drawn = rail.slot(drive, At::Drawn);
            assert_ne!(drawn.y, target.y);
            let margin = if drive == 29 { 0.0 } else { RAIL_PITCH };
            assert!(target.y >= rail.rect.y + margin - 0.01);
            assert!(target.y + target.h <= rail.rect.y + rail.rect.h - margin + 0.01);
            rail.step_motion(Some(drive), true, 0.016);
        }
    }

    fn with_starts(starts: &[usize]) -> RailPart {
        let mut rail = model();
        rail.elems = (0..starts.len() as u32).collect();
        rail.starts = starts.to_vec();
        rail
    }

    #[test]
    fn a_letter_with_no_titles_is_skipped_by_focus_and_dimmed() {
        // A B C D: B and C have no titles (their successors start at the same item).
        let rail = with_starts(&[0, 10, 10, 10]);
        assert!(!rail.letter_empty(0) && rail.letter_empty(1) && rail.letter_empty(2) && !rail.letter_empty(3));
        assert_eq!(EMPTY_LETTER_ALPHA, 0.25);
        let none = |i: usize| rail.skip_empty(i);
        assert_eq!(none(1), 0, "an empty letter resolves to the one before it");
        assert_eq!(rail.letter_for_item(10), 3, "an item lands on the letter that owns it");
        assert_eq!(rail.letter_for_item(9), 0);
        let down = (0 + 1..rail.elems.len()).find(|i| !rail.letter_empty(*i));
        assert_eq!(down, Some(3), "Down from A steps over B and C");
        let up = (0..3).rev().find(|i| !rail.letter_empty(*i));
        assert_eq!(up, Some(0), "Up from D steps back over C and B");
    }

    #[test]
    fn the_rail_is_hidden_at_rest_and_shows_on_fast_scroll_or_the_right_edge() {
        let rest = RailHint::default();
        assert!(!rest.wants_rail(false));
        assert!(rest.wants_rail(true), "focus on a letter keeps it up");
        assert!(RailHint { right_edge: true, ..rest }.wants_rail(false));
        assert!(RailHint { scroll_vel: FAST_SCROLL_VEL + 1.0, ..rest }.wants_rail(false));
        assert!(RailHint { scroll_vel: -(FAST_SCROLL_VEL + 1.0), ..rest }.wants_rail(false), "either direction");
        assert!(!RailHint { scroll_vel: FAST_SCROLL_VEL - 1.0, ..rest }.wants_rail(false), "a single row step is not fast");
    }

    #[test]
    fn a_hidden_rail_at_rest_requests_no_frames() {
        let _guard = plx_base::testlock::serial();
        let mut rail = model();
        for _ in 0..400 { rail.step_motion(Some(0), RailHint::default().wants_rail(false), 0.016); }
        let (_, moving) = plx_machine::idle::scoped_motion(|| rail.step_motion(Some(0), false, 0.016));
        assert!(!moving);
        assert!(rail.alpha.pos.abs() < 0.001);
    }

    #[test]
    fn every_rail_motion_component_enters_the_canonical_state() {
        let hash = |rail: &RailPart| { let mut c = Canon::new(); rail.write(&mut c); c.finish() };
        let initial = hash(&model());
        for component in 0..5 {
            let mut rail = model();
            match component { 0 => rail.scroll.pos = 1.0, 1 => rail.scroll.vel = 1.0,
                2 => rail.scroll_target = 1.0, 3 => rail.alpha.pos = 1.0, _ => rail.alpha.vel = 1.0 }
            assert_ne!(hash(&rail), initial, "motion component {component}");
        }
    }
}
