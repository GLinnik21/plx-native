//! Fixed-band rail owned by MasterDetail. Only Input owns its focused/remembered letter.
use std::ffi::CString;
use super::identity::{KeyRegistry, KeyRegion, region_of_elem};
use super::layout::{rail_geom, rail_pitch, rail_scroll_target, RAIL_TRACK_W};
use crate::registry::{LibraryIdentity, LibraryLike, LibrarySectionIdentity};
use plx_ui::consts::K_SCROLL;
use plx_ui::frame::Budget;
use plx_machine::machine::{Canon, Cx, EntryId, FocusKey, GroupId};
use plx_ui::screen::{Activate, At, AxisMask, Dir, DrawFrame, EdgeRule, ElemKind, Focusable,
    GroupKind, GroupSpec, Hover, Part, Placed, Seat, Step, Stop};
use plx_ui::{Rect, Spring, theme};

/// The focus disc behind a letter: two pixels inside the letter pitch.
fn focus_disc(pitch: f32) -> f32 { pitch - 2.0 }
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

/// Focus on the grid's last column, one Right press from the rail: the rail shows itself ahead of
/// the press (`RailHint::right_edge`).
pub(super) fn on_last_column(grid_index: Option<usize>, cols: usize) -> bool {
    cols > 0 && grid_index.is_some_and(|index| index % cols == cols - 1)
}

/// **The letters the rail shows**: the library's script alphabet merged with what the server
/// listed (`/firstCharacter` omits a letter nothing starts with), so an absent letter is on the
/// rail with count 0 (dimmed, and focus skips it) instead of missing from it.
///
/// A Latin listing (every label `#` or a Latin letter) is `#` (only when the server reported
/// non-letter titles) then A to Z, followed by any other Latin label (an accented letter) in the
/// server's order. Any other script (Cyrillic, Greek, digits, ...) is the server's own list as
/// given: no alphabet is invented for a script the app does not know the order of.
pub(super) fn alphabet(server: &[(String, i64)]) -> Vec<(String, i64)> {
    let latin = !server.is_empty() && server.iter().all(|(label, _)| {
        let mut chars = label.chars();
        matches!((chars.next(), chars.next()), (Some(c), None) if c == '#' || c.is_ascii_alphabetic()
            || ('\u{C0}'..='\u{24F}').contains(&c))
    });
    if !latin { return server.to_vec(); }
    let count_of = |wanted: &str| server.iter().find(|(label, _)| label.eq_ignore_ascii_case(wanted)).map_or(0, |(_, n)| *n);
    let mut out = Vec::with_capacity(27);
    if server.iter().any(|(label, n)| label == "#" && *n > 0) { out.push(("#".to_owned(), count_of("#"))); }
    out.extend(('A'..='Z').map(|c| (c.to_string(), count_of(&c.to_string()))));
    out.extend(server.iter().filter(|(label, _)| {
        label != "#" && !(label.len() == 1 && label.as_bytes()[0].is_ascii_alphabetic())
    }).cloned());
    out
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
        // The jump offsets are prefix sums of the counts, so a letter with no titles adds 0.
        let mut start = 0usize;
        for (index, (label, count)) in alphabet(view.letters()).iter().enumerate() {
            self.starts.push(start);
            start = start.saturating_add((*count).max(0) as usize);
            self.elems.push(keys.register(LibraryIdentity::Rail { section: section.clone(), label: label.clone() }, self.group, index));
            self.labels.push(CString::new(label.as_str()).unwrap_or_default());
        }
        // One entry past the last letter: where the listing ends, so a letter at the END of the
        // alphabet that nothing starts with reads as empty like any other.
        self.starts.push(start);
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

    /// A letter no title starts with (the server did not list it): the next letter, or the end of
    /// the listing, begins at the same item. Dimmed, and focus steps over it.
    pub(super) fn letter_empty(&self, index: usize) -> bool {
        match (self.starts.get(index), self.starts.get(index + 1)) {
            (Some(a), Some(b)) => a == b,
            _ => false,
        }
    }

    fn pitch(&self) -> f32 { rail_pitch(self.elems.len()) }

    /// The rail as a route test reads it: every letter with whether the server listed a title
    /// under it, and the index of the one focus is on (`None` off the rail).
    #[cfg(any(test, feature = "test-support"))]
    pub(super) fn probe(&self, focus: Option<FocusKey<u32>>) -> (Vec<(String, bool)>, Option<usize>) {
        let letters = self.labels.iter().enumerate()
            .map(|(index, label)| (label.to_string_lossy().into_owned(), self.letter_empty(index))).collect();
        let focused = focus.filter(|key| key.entry == self.entry)
            .and_then(|key| self.elems.iter().position(|elem| *elem == key.elem));
        (letters, focused)
    }

    /// The nearest letter with titles at or before `index`, else the first one after it.
    fn skip_empty(&self, index: usize) -> usize {
        (0..=index.min(self.elems.len().saturating_sub(1))).rev().find(|i| !self.letter_empty(*i))
            .or_else(|| (index..self.elems.len()).find(|i| !self.letter_empty(*i)))
            .unwrap_or(index)
    }

    pub(super) fn letter_for_item(&self, index: usize) -> usize {
        // (the trailing entry is the listing's end, not a letter)
        let letters = self.elems.len();
        self.starts.iter().take(letters).enumerate().rev().find(|(_, start)| **start <= index).map(|(i, _)| i).unwrap_or(0)
    }

    pub(super) fn start_for_elem(&self, elem: u32) -> Option<usize> {
        self.elems.iter().position(|key| *key == elem).and_then(|index| self.starts.get(index).copied())
    }

    fn slot(&self, index: usize, at: At) -> Rect {
        let scroll = match at {
            At::Drawn => self.scroll.pos,
            At::SpringTarget => rail_scroll_target(self.scroll.pos, index, self.elems.len()),
        };
        Rect::new(self.rect.x, self.rect.y + index as f32 * self.pitch() - scroll, RAIL_TRACK_W, self.pitch())
    }

    fn letter_alpha(&self, index: usize) -> f32 {
        let rel = self.slot(index, At::Drawn).cy() - self.rect.y;
        let max = rail_geom(self.elems.len()).3;
        let mut alpha = 1.0f32;
        if self.scroll.pos > 0.5 { alpha = alpha.min(rel / self.pitch()); }
        if self.scroll.pos < max - 0.5 { alpha = alpha.min((self.rect.h - rel) / self.pitch()); }
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
                let size = focus_disc(self.pitch());
                let disc = Rect::new(slot.cx() - size * 0.5, slot.cy() - size * 0.5, size, size);
                q.rect(disc, size * 0.5, plx_ui::ACCENT, plx_ui::ACCENT, 0.0);
                plx_ui::ACCENT_INK
            } else if current == Some(index) { theme::TEXT_PRIMARY } else { theme::TEXT_SECONDARY };
            plx_ui::label::Label::new(label.as_ptr(), theme::size::MICRO, ink)
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
    use crate::registry::{AppFx, AppMsg, PageMemory};

    fn model() -> RailPart {
        let mut rail = RailPart::new(EntryId(1), GroupId(2));
        rail.elems = (0..40).collect();
        let (y, x, h, _) = rail_geom(40);
        rail.rect = Rect::new(x - RAIL_TRACK_W * 0.5, y, RAIL_TRACK_W, h);
        rail
    }

    #[test]
    fn rail_scroll_and_presence_animate_then_settle_outside_the_grid_region() {
        let _guard = plx_base::testlock::serial();
        let mut rail = model();
        let (_, moving) = plx_machine::idle::scoped_motion(|| rail.step_motion(Some(39), true, 0.016));
        assert!(moving, "entering the region reports its scroll and fade motion");
        assert!(rail.scroll.pos > 0.0 && rail.scroll.pos < rail.scroll_target);
        assert!(rail.alpha.pos > 0.0 && rail.alpha.pos < 1.0);
        for _ in 0..400 { rail.step_motion(Some(39), true, 0.016); }
        assert!((rail.scroll.pos - rail_geom(40).3).abs() < 0.001);
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
        rail.step_motion(Some(37), true, 0.016);
        for drive in [38, 39] {
            let target = rail.slot(drive, At::SpringTarget);
            let drawn = rail.slot(drive, At::Drawn);
            assert_ne!(drawn.y, target.y);
            let margin = if drive == 39 { 0.0 } else { rail.pitch() };
            assert!(target.y >= rail.rect.y + margin - 0.01);
            assert!(target.y + target.h <= rail.rect.y + rail.rect.h - margin + 0.01);
            rail.step_motion(Some(drive), true, 0.016);
        }
    }

    fn with_starts(starts: &[usize]) -> RailPart {
        let mut rail = model();
        rail.elems = (0..starts.len() as u32).collect();
        // the listing ends one title past the last letter's start
        rail.starts = starts.iter().copied().chain([starts.last().map_or(0, |last| last + 1)]).collect();
        rail
    }

    struct RailHost;
    #[derive(Clone, Copy)]
    struct NoViews<'a> {
        listing: plx_data::stores::browse::ListingView<'a>,
        directory: plx_data::stores::browse::DirectoryView<'a>,
        hubs: plx_data::stores::browse::HubsView<'a>,
    }
    impl plx_machine::machine::Host for RailHost {
        type Arg = plx_ui::fixture::FixtureArg;
        type Fx = AppFx;
        type Msg = AppMsg;
        type Elem = u32;
        type Views<'a> = NoViews<'a>;
        type Init = plx_ui::fixture::FixtureArg;
        type Memory = PageMemory;
    }
    impl LibraryLike for RailHost {
        fn listing<'a>(cx: &Cx<'a, Self>) -> plx_data::stores::browse::ListingView<'a> { cx.views.listing }
        fn directory<'a>(cx: &Cx<'a, Self>) -> plx_data::stores::browse::DirectoryView<'a> { cx.views.directory }
        fn section_hubs<'a>(cx: &Cx<'a, Self>) -> plx_data::stores::browse::HubsView<'a> { cx.views.hubs }
    }

    #[test]
    fn a_letter_with_no_titles_is_skipped_by_focus_and_dimmed() {
        use plx_machine::machine::{FocusRead, InputOwner, PressRead, Tick};
        // A B C D: B and C have no titles (their successors start at the same item).
        let rail = with_starts(&[0, 10, 10, 10]);
        assert!(!rail.letter_empty(0) && rail.letter_empty(1) && rail.letter_empty(2) && !rail.letter_empty(3));
        assert_eq!(EMPTY_LETTER_ALPHA, 0.25);
        let directory = plx_data::stores::browse::DirectoryView::empty_for_test();
        let (listing, hubs) = (plx_data::stores::browse::ListingSnapshot::empty_for_test(), plx_data::stores::browse::HubsSnapshot::empty_for_test());
        let cx = Cx::<RailHost> {
            views: NoViews { listing: listing.view(), directory, hubs: hubs.view() },
            tick: Tick::default(), measure: &plx_ui::fixture::FixtureMeasure, press: PressRead::default(),
            focus: FocusRead::default(), owner: InputOwner::Entry(rail.entry),
        };
        let key = |i: usize| FocusKey { entry: rail.entry, elem: rail.elems[i] };
        assert!(matches!(rail.neighbour(key(0), Dir::Down, &cx), Step::Move(k) if k == key(3)), "Down from A steps over B and C");
        assert!(matches!(rail.neighbour(key(3), Dir::Up, &cx), Step::Move(k) if k == key(0)), "Up from D steps back over C and B");
        assert!(matches!(rail.neighbour(key(3), Dir::Down, &cx), Step::Edge));
        let placed = |item: u32| Placed { rect: rail.rect, rest_rect: rail.rect, clip: rail.rect, index: Some(item) };
        assert_eq!(rail.seat(rail.group, placed(10), &cx), key(3), "an item lands on the letter that owns it");
        assert_eq!(rail.seat(rail.group, placed(9), &cx), key(0));
        // A seat that lands on an empty letter (an item before the first start) resolves forward
        // to the first letter that has titles.
        let leading_empty = with_starts(&[10, 10, 20]);
        assert!(leading_empty.letter_empty(0));
        assert_eq!(leading_empty.seat(leading_empty.group, placed(5), &cx), FocusKey { entry: leading_empty.entry, elem: leading_empty.elems[1] });
    }

    #[test]
    fn a_letter_at_the_end_of_the_alphabet_with_no_titles_is_empty_too() {
        // A and B have titles, C and D do not: the listing ends where C would begin.
        let mut rail = RailPart::new(EntryId(1), GroupId(2));
        rail.elems = (0..4).collect();
        rail.starts = vec![0, 3, 5, 5, 5];
        assert!(!rail.letter_empty(0) && !rail.letter_empty(1) && rail.letter_empty(2) && rail.letter_empty(3));
        assert_eq!(rail.letter_for_item(4), 1, "the last item belongs to B, never to an empty trailing letter");
        assert_eq!(rail.skip_empty(3), 1);
    }

    fn server(letters: &[(&str, i64)]) -> Vec<(String, i64)> {
        letters.iter().map(|(l, n)| ((*l).to_owned(), *n)).collect()
    }

    /// The server's letters with the counts of the alphabet it was merged into.
    fn labels_of(list: &[(String, i64)]) -> String { list.iter().map(|(l, _)| l.as_str()).collect::<Vec<_>>().join("") }

    #[test]
    fn a_latin_listing_is_the_whole_alphabet_with_absent_letters_at_zero() {
        // What PMS answers for a small library: only the letters something starts with.
        let list = alphabet(&server(&[("B", 3), ("C", 2), ("E", 1), ("S", 4), ("W", 5)]));
        assert_eq!(labels_of(&list), "ABCDEFGHIJKLMNOPQRSTUVWXYZ", "no # when the server reported no non-letter titles");
        assert_eq!(list.len(), 26);
        let count = |l: &str| list.iter().find(|(x, _)| x == l).unwrap().1;
        assert_eq!((count("A"), count("B"), count("D"), count("S"), count("Z")), (0, 3, 0, 4, 0));
        assert_eq!(list.iter().map(|(_, n)| n).sum::<i64>(), 15, "no title is invented or lost");
    }

    #[test]
    fn a_hash_is_kept_first_only_when_the_server_reported_non_letter_titles() {
        let list = alphabet(&server(&[("#", 2), ("A", 1), ("Z", 1)]));
        assert_eq!(labels_of(&list), "#ABCDEFGHIJKLMNOPQRSTUVWXYZ");
        assert_eq!(list.len(), 27);
        assert_eq!(list[0], ("#".to_owned(), 2));
    }

    #[test]
    fn a_script_other_than_latin_is_the_servers_own_letters_and_nothing_is_invented() {
        let cyrillic = server(&[("А", 3), ("Б", 1), ("Я", 2)]);
        assert_eq!(alphabet(&cyrillic), cyrillic);
        let digits = server(&[("0", 1), ("1", 1)]);
        assert_eq!(alphabet(&digits), digits);
        assert!(alphabet(&[]).is_empty());
    }

    #[test]
    fn jump_offsets_are_prefix_sums_that_an_empty_letter_adds_nothing_to() {
        let list = alphabet(&server(&[("B", 3), ("E", 2)]));
        let mut start = 0usize;
        let starts: Vec<usize> = list.iter().map(|(_, n)| { let s = start; start += *n as usize; s }).collect();
        // A B C D E F: A has none, B starts at 0, C and D have none (3), E starts at 3, F at 5.
        assert_eq!(&starts[..6], &[0, 0, 3, 3, 3, 5]);
        assert_eq!(starts[25], 5);
    }

    #[test]
    fn the_rail_is_the_boards_column_at_pitch_32_starting_higher_before_it_shrinks() {
        use super::super::layout::{RAIL_CAP_PAD, RAIL_MIN_PITCH, RAIL_PITCH, RAIL_CX};
        assert_eq!((RAIL_PITCH, RAIL_TRACK_W, RAIL_CX), (32.0, 44.0, 1844.0));
        for n in [9usize, 26, 27] {
            let (y, x, h, max) = rail_geom(n);
            let pitch = rail_pitch(n);
            assert_eq!((x, max), (1844.0, 0.0), "{n} letters: the whole alphabet stands, no scrolling");
            assert!((h - n as f32 * pitch).abs() < 0.01);
            assert!(y + h + RAIL_CAP_PAD <= plx_ui::consts::SCR_H - plx_ui::consts::MARGIN_Y + 0.01, "{n} letters end inside the bottom margin");
            assert!((RAIL_MIN_PITCH..=RAIL_PITCH).contains(&pitch));
        }
        assert_eq!(rail_pitch(27), 32.0, "# and A to Z keep the full 32 pitch");
        let (top27, ..) = rail_geom(27);
        assert!(top27 < 244.0, "27 letters start above the grid top ({top27}) rather than shrink");
        assert_eq!(rail_geom(9).0, 244.0, "a short alphabet starts at the grid top");
        assert!(rail_geom(60).3 > 0.0, "a script too long even at the least pitch still scrolls");
    }

    #[test]
    fn the_letters_stand_bare_regular_and_centred_on_1844_with_no_track() {
        use plx_machine::machine::{FocusRead, InputOwner, PressRead, Tick};
        let _guard = plx_base::testlock::serial();
        let mut rail = with_starts(&[0, 1, 2]);
        rail.labels = ["#", "A", "B"].iter().map(|l| CString::new(*l).unwrap()).collect();
        let (y, x, h, _) = rail_geom(3);
        rail.rect = Rect::new(x - RAIL_TRACK_W * 0.5, y, RAIL_TRACK_W, h);
        rail.alpha = Spring::at(1.0);
        let directory = plx_data::stores::browse::DirectoryView::empty_for_test();
        let (listing, hubs) = (plx_data::stores::browse::ListingSnapshot::empty_for_test(), plx_data::stores::browse::HubsSnapshot::empty_for_test());
        let cx = Cx::<RailHost> {
            views: NoViews { listing: listing.view(), directory, hubs: hubs.view() },
            tick: Tick::default(), measure: &plx_ui::fixture::FixtureMeasure, press: PressRead::default(),
            focus: FocusRead::default(), owner: InputOwner::Entry(rail.entry),
        };
        let mut frame = DrawFrame::new(&cx, plx_ui::Painter::recording());
        let (runs, rects) = {
            let mut rects = Vec::new();
            let runs = plx_ui::draw_census::capture_runs(|| {
                rects = plx_ui::draw_census::capture(|| rail.draw_with_current(&mut frame, Rect::new(0.0, 0.0, 1920.0, 1080.0), None));
            });
            (runs, rects)
        };
        assert!(rects.iter().all(|(_, r)| r.w < RAIL_TRACK_W - 1.0 || r.h < RAIL_TRACK_W), "no dark track plate is drawn: {rects:?}");
        assert_eq!(runs.len(), 3);
        for r in &runs {
            assert!(!r.bold, "letters are regular: {}", r.text);
            assert_eq!(r.size, theme::size::MICRO);
        }
        for r in &runs {
            assert_ne!(r.align, 0, "{} is centre-anchored", r.text);
            assert!((r.at.x - 1844.0).abs() < 0.6, "{} anchored on 1844, got {}", r.text, r.at.x);
        }
        assert!((runs[1].at.y - runs[0].at.y - 32.0).abs() < 0.01, "pitch 32");
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
