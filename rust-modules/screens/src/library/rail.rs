//! The rail owned by MasterDetail: the A-Z letters of a title sort, or the BANDS of a value sort
//! (`95% 90% ... <75%`, `Rating`, `Duration`, `Unplayed`, `Plays`, and the calendar sorts `Today Oct
//! ... Jan 2025 ...` and `2026 2020s ...`). Only Input owns its focused or remembered element.
//!
//! One part serves both because the logic is the same: an element is a place in the listing
//! (`starts`, the index of its first item, with one trailing entry where the last one ends), an element
//! no item starts is dimmed and focus steps over it, and a move inside the rail scrolls the grid to the
//! element's first card at once (`LibraryScreen::follow`). What differs is only where the elements come
//! from (`/firstCharacter` counts, or the data job's band offsets), their labels, their geometry
//! ([`BandRows`]: a 40 px pitch, centred on 1834, one 8 px step where a calendar's scale changes, in a
//! column as wide as its widest label) and what
//! wears the focus (a disc behind a letter, a pill around a band's label). The items a value sort
//! leaves out (no rating, never viewed) lie before the first band or after the last and belong to
//! none, so a grid card among them marks no current band.
use std::ffi::CString;
use super::identity::{KeyRegistry, KeyRegion, region_of_elem};
use super::layout::{band_pill_pad, band_track_w, rail_geom, rail_pitch, rail_scroll_target, BandRows, BAND_PILL_H, RAIL_TRACK_W};
use super::rail_bands;
use crate::registry::{LibraryIdentity, LibraryLike, LibrarySectionIdentity};
use plx_ui::consts::K_SCROLL;
use plx_ui::frame::Budget;
use plx_machine::machine::{Canon, Cx, EntryId, FocusKey, GroupId};
use plx_ui::screen::{Activate, At, AxisMask, Dir, DrawFrame, EdgeRule, ElemKind, Focusable,
    GroupKind, GroupSpec, Hover, Part, Placed, Seat, Step, Stop};
use plx_ui::{Rect, Spring, theme};
use plx_ui::widgets::{focus_pill, PILL_FOCUS_OUTSET};

/// The focus disc behind a letter: two pixels inside the letter pitch.
fn focus_disc(pitch: f32) -> f32 { pitch - 2.0 }
/// The material of the focused element's ground, a letter's disc and a band's pill alike: the app's
/// white focus fill, fully rounded.
fn focus_fill(p: plx_ui::Painter, rect: Rect) {
    p.rect(rect, rect.h * 0.5, plx_ui::ACCENT, plx_ui::ACCENT, 0.0);
}

/// The focused band's pill, centred on its row `slot`: the board's 38 high with 12 px each side of the
/// label, less for a label too wide for that (`band_pill_pad`), built as the shared [`focus_pill`]
/// around the plate it grows from.
fn band_pill(slot: Rect, label_w: f32) -> Rect {
    let plate_w = label_w + 2.0 * (band_pill_pad(label_w) - PILL_FOCUS_OUTSET);
    let plate_h = BAND_PILL_H - 2.0 * PILL_FOCUS_OUTSET;
    focus_pill(Rect::new(slot.cx() - plate_w * 0.5, slot.cy() - plate_h * 0.5, plate_w, plate_h))
}

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

/// The column of a rail of bands, derived from the listing's band offsets and the labels' measured
/// widths whenever the rail is refreshed: nothing here is state of its own, so it is not part of the
/// rail's canonical form (the labels, starts and rect it yields are).
#[derive(Clone, Debug, PartialEq)]
struct BandColumn {
    rows: BandRows,
    /// Each label's measured width, for the pill around the focused one.
    widths: Vec<f32>,
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
    /// `Some` while the rail shows a value sort's bands; `None` while it shows letters.
    bands: Option<BandColumn>,
}

impl RailPart {
    pub(super) const SHAPE: &'static str = "LibraryRail{group:u32,elems:[u32],labels:[str],starts:[u32],rect:{x:f32,y:f32,w:f32,h:f32},scroll:Spring{pos:f32,vel:f32},scroll_target:f32,alpha:Spring{pos:f32,vel:f32}}";

    pub(super) fn new(entry: EntryId, group: GroupId) -> Self {
        Self { entry, group, elems: Vec::new(), labels: Vec::new(), starts: Vec::new(),
            rect: Rect::new(0.0, 0.0, 0.0, 0.0), scroll: Spring::at(0.0),
            scroll_target: 0.0, alpha: Spring::at(0.0), bands: None }
    }

    pub(super) fn clear_projection(&mut self) {
        self.elems.clear(); self.labels.clear(); self.starts.clear(); self.bands = None;
        self.scroll.jump(0.0); self.scroll_target = 0.0;
    }

    pub(super) fn write(&self, c: &mut Canon) {
        let Self { entry: _, group, elems, labels, starts, rect, scroll, scroll_target, alpha, bands: _ } = self;
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
        self.elems.clear(); self.labels.clear(); self.starts.clear(); self.bands = None;
        // A title sort has the letters, a value sort its bands, and the two never both.
        let letters = view.rail_available();
        if view.total() <= 0 || !(letters || view.bands_available()) { self.clear_projection(); return; }
        let section = LibrarySectionIdentity { sid: id.sid, key: id.section };
        if !letters { self.refresh_bands(cx, keys, &section); return; }
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

    /// The bands of the listing's value sort, in the listing's own order. A band's `starts` entry is
    /// its first item's index (the offsets already hold the running sums, with the no-value items
    /// before the first band or after the last), and the trailing entry is where the last band ends,
    /// so the items beyond it are in no band. Focus identity is the band's token, not its label: it
    /// survives a language change and never collides with a letter of the same text.
    fn refresh_bands<H: LibraryLike>(&mut self, cx: &Cx<'_, H>, keys: &mut KeyRegistry, section: &LibrarySectionIdentity) {
        let Some(offsets) = H::listing(cx).bands() else { self.clear_projection(); return };
        let marks: Vec<_> = offsets.bands.iter().map(|band| band.band.mark()).collect();
        let mut widths = Vec::with_capacity(marks.len());
        for (index, band) in offsets.bands.iter().enumerate() {
            self.starts.push(band.start as usize);
            let identity = rail_bands::identity(band.band.mark());
            self.elems.push(keys.register(LibraryIdentity::Rail { section: section.clone(), label: identity }, self.group, index));
            let label = CString::new(rail_bands::label(band.band.mark())).unwrap_or_default();
            widths.push(cx.measure.width(&label, theme::size::MICRO, false));
            self.labels.push(label);
        }
        self.starts.push(offsets.bands.last().map_or(0, |last| (last.start + last.size) as usize));
        let rows = BandRows::new(self.elems.len(), &rail_bands::steps(&marks));
        let (y, cx_rail, height, max) = rows.geom();
        let track_w = band_track_w(widths.iter().copied().fold(0.0, f32::max));
        self.rect = Rect::new(cx_rail - track_w * 0.5, y, track_w, height);
        self.scroll_target = self.scroll_target.clamp(0.0, max);
        self.bands = Some(BandColumn { rows, widths });
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
            .or_else(|| grid_index.and_then(|index| self.current_for_item(index)));
        let in_rail = cx.focus.current.filter(|key| key.entry == self.entry)
            .is_some_and(|key| region_of_elem(key.elem) == Some(KeyRegion::Rail));
        let active = live && !self.elems.is_empty() && self.in_region(cx) && hint.wants_rail(in_rail);
        self.step_motion(drive, active, dt);
    }

    fn step_motion(&mut self, drive: Option<usize>, active: bool, dt: f32) {
        if let Some(drive) = drive {
            self.scroll_target = self.target_for(self.scroll.pos, drive);
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

    /// The distance between two elements' tops.
    fn pitch(&self) -> f32 {
        self.bands.as_ref().map_or_else(|| rail_pitch(self.elems.len()), |_| super::layout::BAND_PITCH)
    }

    /// `(top, centre x, window height, scroll range)` of the column, whichever it holds.
    fn geom(&self) -> (f32, f32, f32, f32) {
        self.bands.as_ref().map_or_else(|| rail_geom(self.elems.len()), |column| column.rows.geom())
    }

    /// Where element `index`'s row starts inside the column.
    fn row_top(&self, index: usize) -> f32 {
        self.bands.as_ref().map_or_else(|| index as f32 * self.pitch(), |column| column.rows.top(index as isize))
    }

    /// The scroll that brings element `drive` into view from `scroll`.
    fn target_for(&self, scroll: f32, drive: usize) -> f32 {
        self.bands.as_ref().map_or_else(|| rail_scroll_target(scroll, drive, self.elems.len()),
            |column| column.rows.scroll_target(scroll, drive))
    }

    /// The rail as a route test reads it: every letter or band with whether the server listed a title
    /// under it, and the index of the one focus is on (`None` off the rail).
    #[cfg(any(test, feature = "test-support"))]
    pub(super) fn probe(&self, focus: Option<FocusKey<u32>>) -> (Vec<(String, bool)>, Option<usize>) {
        let letters = self.labels.iter().enumerate()
            .map(|(index, label)| (label.to_string_lossy().into_owned(), self.letter_empty(index))).collect();
        let focused = focus.filter(|key| key.entry == self.entry)
            .and_then(|key| self.elems.iter().position(|elem| *elem == key.elem));
        (letters, focused)
    }

    /// The nearest letter or band with titles at or before `index`, else the first one after it.
    fn skip_empty(&self, index: usize) -> usize {
        (0..=index.min(self.elems.len().saturating_sub(1))).rev().find(|i| !self.letter_empty(*i))
            .or_else(|| (index..self.elems.len()).find(|i| !self.letter_empty(*i)))
            .unwrap_or(index)
    }

    /// The element the grid item `index` is under, for the ink of the CURRENT one: a letter is always
    /// under one, a band is not when the item is among those the sort leaves without a value.
    pub(super) fn current_for_item(&self, index: usize) -> Option<usize> {
        if self.bands.is_none() { return Some(self.letter_for_item(index)); }
        (0..self.elems.len()).find(|i| matches!((self.starts.get(*i), self.starts.get(i + 1)),
            (Some(a), Some(b)) if (*a..*b).contains(&index)))
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
            At::SpringTarget => self.target_for(self.scroll.pos, index),
        };
        Rect::new(self.rect.x, self.rect.y + self.row_top(index) - scroll, self.rect.w, self.pitch())
    }

    fn letter_alpha(&self, index: usize) -> f32 {
        let rel = self.slot(index, At::Drawn).cy() - self.rect.y;
        let max = self.geom().3;
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
        let _clip = (self.geom().3 > 0.5).then(|| f.clip(p, self.rect));
        let current = grid_index.and_then(|index| self.current_for_item(index));
        for (index, label) in self.labels.iter().enumerate() {
            let alpha = self.letter_alpha(index)
                * if self.letter_empty(index) { EMPTY_LETTER_ALPHA } else { 1.0 };
            if alpha <= 0.0 { continue; }
            let q = p.alpha(alpha);
            let slot = self.slot(index, At::Drawn);
            let focused = f.focus.current == Some(FocusKey { entry: self.entry, elem: self.elems[index] });
            let ink = if focused {
                match &self.bands {
                    None => {
                        let size = focus_disc(self.pitch());
                        focus_fill(q, Rect::new(slot.cx() - size * 0.5, slot.cy() - size * 0.5, size, size));
                    }
                    Some(column) => focus_fill(q, band_pill(slot, column.widths.get(index).copied().unwrap_or(0.0))),
                }
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
            // A key the rail no longer holds lands on the first element focus can stop on: on a calendar
            // rail the first is `Today`, often an empty band `neighbour` never rests on.
            self.elems.get(self.skip_empty(0)).map(|elem| FocusKey { entry: self.entry, elem: *elem }).unwrap_or(want)
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

    // ---- the rail of a value sort's bands -------------------------------------------------------

    use plx_data::browse::bands::{BandFamily, BandRail};
    use plx_machine::machine::{FocusRead, InputOwner, PressRead, Tick};

    /// A listing of `total` films sorted by `key`, whose `family` counts (`at_or_above`, highest band
    /// first) were landed for it.
    struct BandRig {
        listing: plx_data::stores::browse::ListingSnapshot,
        hubs: plx_data::stores::browse::HubsSnapshot,
    }

    impl BandRig {
        fn new(family: BandFamily, key: &str, at_or_above: &[u32], total: u32, descending: bool) -> Self {
            let sid = plx_plex::plex::ServerId::from_raw(0);
            let ladder = family.fixed_ladder().expect("a fixed ladder");
            let rail = BandRail::new(&ladder, at_or_above, total).expect("trusted counts");
            let items = (0..total).map(|i| Some(plx_data::pms::PmsMovie { sid, rk: format!("{i}"), ..Default::default() })).collect();
            let listing = plx_data::browse::view::ListingSnapshot::fixture(sid, items, Vec::new())
                .with_sort_key(key).with_sort_desc(descending).with_bands(rail);
            Self { listing, hubs: plx_data::stores::browse::HubsSnapshot::empty_for_test() }
        }

        /// 40 films by Critic rating: 95% holds 2, 90% 3, 85% 7, 80% none, 75% 8, `<75%` 14, and six have
        /// no rating at all.
        fn rating(descending: bool) -> Self {
            Self::new(BandFamily::Rating, "rating", &[2, 5, 12, 12, 20, 34], 40, descending)
        }

        fn cx<'a>(&'a self, entry: EntryId, focus: Option<FocusKey<u32>>) -> Cx<'a, RailHost> {
            Cx::<RailHost> {
                views: NoViews { listing: self.listing.view(), directory: plx_data::stores::browse::DirectoryView::empty_for_test(), hubs: self.hubs.view() },
                tick: Tick::default(), measure: &plx_ui::fixture::FixtureMeasure, press: PressRead::default(),
                focus: FocusRead { current: focus, ..FocusRead::default() }, owner: InputOwner::Entry(entry),
            }
        }

        fn rail(&self) -> RailPart {
            let mut rail = RailPart::new(EntryId(1), GroupId(2));
            rail.refresh(&self.cx(rail.entry, None), &mut KeyRegistry::default());
            rail.alpha = Spring::at(1.0);
            rail
        }
    }

    fn texts(rail: &RailPart) -> Vec<String> {
        rail.labels.iter().map(|label| label.to_string_lossy().into_owned()).collect()
    }

    #[test]
    fn a_descending_rating_sort_lists_its_bands_top_down_and_dims_the_empty_one() {
        let _guard = plx_base::testlock::serial();
        let rig = BandRig::rating(true);
        let rail = rig.rail();
        assert_eq!(texts(&rail), ["95%", "90%", "85%", "80%", "75%", "<75%"]);
        assert_eq!(rail.starts, [0, 2, 5, 12, 12, 20, 34], "each band's first item, then where the last band ends");
        let empty: Vec<usize> = (0..6).filter(|i| rail.letter_empty(*i)).collect();
        assert_eq!(empty, [3], "only the 80% band holds nothing");
        assert!(rail.bands.is_some(), "the rail is in bands mode");
        let cx = rig.cx(rail.entry, None);
        let key = |i: usize| FocusKey { entry: rail.entry, elem: rail.elems[i] };
        assert!(matches!(rail.neighbour(key(2), Dir::Down, &cx), Step::Move(k) if k == key(4)), "Down from 85% steps over 80%");
        assert!(matches!(rail.neighbour(key(4), Dir::Up, &cx), Step::Move(k) if k == key(2)), "Up from 75% steps back over 80%");
        assert!(matches!(rail.neighbour(key(5), Dir::Down, &cx), Step::Edge));
        assert!(matches!(rail.neighbour(key(0), Dir::Up, &cx), Step::Edge));
    }

    #[test]
    fn an_ascending_sort_reads_the_bands_in_listing_order_with_the_unrated_items_first() {
        let _guard = plx_base::testlock::serial();
        let rig = BandRig::rating(false);
        let rail = rig.rail();
        assert_eq!(texts(&rail), ["<75%", "75%", "80%", "85%", "90%", "95%"]);
        assert_eq!(rail.starts, [6, 20, 28, 28, 35, 38, 40], "six unrated items lead; the 95% band ends the listing");
        assert!((0..6).all(|item| rail.current_for_item(item).is_none()), "an unrated item is in no band");
        assert_eq!(rail.current_for_item(6), Some(0));
        assert_eq!(rail.current_for_item(39), Some(5));
    }

    #[test]
    fn an_item_with_no_value_marks_no_current_band_and_every_other_item_marks_its_own() {
        let _guard = plx_base::testlock::serial();
        let rail = BandRig::rating(true).rail();
        let current: Vec<Option<usize>> = [0, 1, 2, 4, 5, 11, 12, 19, 20, 33, 34, 39].iter().map(|i| rail.current_for_item(*i)).collect();
        // the 80% band is empty, so the items it would hold are the 75% band's
        assert_eq!(current, [Some(0), Some(0), Some(1), Some(1), Some(2), Some(2), Some(4), Some(4), Some(5), Some(5), None, None]);
        let letters = with_starts(&[0, 10, 10, 10]);
        assert_eq!(letters.current_for_item(25), Some(3), "a letter rail is under its last letter for any item");
    }

    #[test]
    fn the_seat_from_a_grid_card_lands_on_its_band_or_the_nearest_one_that_holds_titles() {
        let _guard = plx_base::testlock::serial();
        let rig = BandRig::rating(true);
        let rail = rig.rail();
        let cx = rig.cx(rail.entry, None);
        let placed = |item: u32| Placed { rect: rail.rect, rest_rect: rail.rect, clip: rail.rect, index: Some(item) };
        let band = |i: usize| FocusKey { entry: rail.entry, elem: rail.elems[i] };
        assert_eq!(rail.seat(rail.group, placed(6), &cx), band(2));
        assert_eq!(rail.seat(rail.group, placed(12), &cx), band(4), "the 80% band is empty: its first item is 75%'s");
        assert_eq!(rail.seat(rail.group, placed(38), &cx), band(5), "an unrated item seats on the last band");
        let ascending = BandRig::rating(false);
        let lead = ascending.rail();
        let first = lead.seat(lead.group, placed(2), &ascending.cx(lead.entry, None));
        assert_eq!(first, FocusKey { entry: lead.entry, elem: lead.elems[0] }, "a leading unrated item seats on the first band");
    }

    #[test]
    fn a_pill_around_a_wide_label_gives_air_back_and_stays_inside_the_track() {
        let _guard = plx_base::testlock::serial();
        for label_w in [60.0f32, 100.0, 111.0, 113.0, 116.0] {
            let track = band_track_w(label_w);
            let pill = band_pill(Rect::new(0.0, 0.0, track, 40.0), label_w);
            assert!((pill.w - (label_w + 2.0 * band_pill_pad(label_w))).abs() < 0.01, "{label_w}: pill {} wide", pill.w);
            assert!(pill.x >= -0.01 && pill.x + pill.w <= track + 0.01, "{label_w}: the pill {pill:?} leaves its {track} track");
            assert!((pill.cx() - track * 0.5).abs() < 0.01 && pill.h == BAND_PILL_H, "{label_w}: centred and 38 high");
        }
        assert!((band_pill(Rect::new(0.0, 0.0, 84.0, 40.0), 60.0).w - 84.0).abs() < 0.01, "a label that fits keeps the board's 12 each side");
    }

    #[test]
    fn the_bands_stand_on_1834_forty_apart_from_the_grid_top_in_a_track_as_wide_as_its_widest_pill() {
        use super::super::layout::{BAND_PILL_PAD, BAND_PITCH, BAND_RAIL_CX};
        let _guard = plx_base::testlock::serial();
        let rig = BandRig::rating(true);
        let rail = rig.rail();
        assert_eq!((BAND_RAIL_CX, BAND_PITCH), (1834.0, 40.0));
        assert_eq!(rail.rect.y, 244.0, "the first band starts at the grid's top");
        assert!((rail.rect.x + rail.rect.w * 0.5 - 1834.0).abs() < 0.01, "centred on 1834: {:?}", rail.rect);
        // the fixture measures a byte as half the size: `<75%` is 4 x 11 = 44 wide
        assert_eq!(rail.rect.w, 44.0 + 2.0 * BAND_PILL_PAD, "wider than the letters' 44 once a pill must fit");
        assert_eq!(rail.rect.h, 6.0 * 40.0);
        for i in 0..6 {
            assert_eq!(rail.slot(i, At::Drawn).y, 244.0 + i as f32 * 40.0);
            assert_eq!(rail.slot(i, At::Drawn).h, 40.0);
        }
        assert!(rail.rect.x + rail.rect.w <= 1920.0);
        // the group the engine travels is the same box
        let focus = Some(FocusKey { entry: rail.entry, elem: rail.elems[0] });
        assert_eq!(region_of_elem(rail.elems[0]), Some(KeyRegion::Rail));
        let mut groups = Vec::new();
        <RailPart as Focusable<RailHost>>::groups(&rail, &rig.cx(rail.entry, focus), &mut groups);
        assert_eq!(groups.first().map(|g| g.extent), Some(rail.rect));
    }

    #[test]
    fn a_ladder_of_other_text_widens_the_track_to_fit_and_the_letters_rail_keeps_its_own_column() {
        use plx_ui::fixture::FixtureMeasure;
        use plx_machine::machine::Measure;
        let _guard = plx_base::testlock::serial();
        let rig = BandRig::new(BandFamily::Plays, "viewCount", &[1, 2, 3, 4, 5], 9, true);
        let rail = rig.rail();
        assert_eq!(texts(&rail), ["10+", "5", "3", "2", "1"]);
        let widest = FixtureMeasure.width(c"10+", theme::size::MICRO, false);
        assert_eq!(rail.rect.w, widest + 24.0);
        let mut letters = with_starts(&[0, 1, 2]);
        let (y, x, h, _) = rail_geom(3);
        letters.rect = Rect::new(x - RAIL_TRACK_W * 0.5, y, RAIL_TRACK_W, h);
        assert!(letters.bands.is_none());
        assert_eq!((letters.geom().1, letters.rect.w, letters.pitch()), (1844.0, 44.0, 32.0));
    }

    type Drawn = (Vec<plx_ui::draw_census::Run>, Vec<(u64, Rect)>);

    /// What the rail draws with focus on band `focus` and the grid's item under band `current`.
    fn drawn(rig: &BandRig, rail: &RailPart, focus: Option<usize>, current: Option<usize>) -> Drawn {
        let focus = focus.map(|i| FocusKey { entry: rail.entry, elem: rail.elems[i] });
        let cx = rig.cx(rail.entry, focus);
        let mut frame = DrawFrame::new(&cx, plx_ui::Painter::recording());
        let mut shapes = Vec::new();
        let runs = plx_ui::draw_census::capture_runs(|| {
            shapes = plx_ui::draw_census::capture(|| rail.draw_with_current(&mut frame, Rect::new(0.0, 0.0, 1920.0, 1080.0), current));
        });
        (runs, shapes)
    }

    #[test]
    fn bands_wear_present_empty_current_and_focused_ink_and_no_bar_is_drawn() {
        let _guard = plx_base::testlock::serial();
        let rig = BandRig::rating(true);
        let rail = rig.rail();
        // 90% holds the item the grid is on (item 3); focus is on 85% (the engine's key, not the grid's)
        let (runs, shapes) = drawn(&rig, &rail, Some(2), Some(3));
        assert_eq!(runs.len(), 6);
        let ink = |token: [f32; 4], alpha: f32| [token[0], token[1], token[2], token[3] * alpha];
        let near = |a: [f32; 4], b: [f32; 4]| a.iter().zip(b).all(|(x, y)| (x - y).abs() < 0.002);
        let want = [
            ink(theme::TEXT_SECONDARY, 1.0),                    // 95%: present
            ink(theme::TEXT_PRIMARY, 1.0),                      // 90%: the grid's item is here
            ink(plx_ui::ACCENT_INK, 1.0),                       // 85%: focused, on its pill
            ink(theme::TEXT_SECONDARY, EMPTY_LETTER_ALPHA),     // 80%: empty
            ink(theme::TEXT_SECONDARY, 1.0),
            ink(theme::TEXT_SECONDARY, 1.0),
        ];
        for (index, (run, want)) in runs.iter().zip(want).enumerate() {
            assert!(near(run.ink, want), "band {index} {:?}: ink {:?}, wanted {want:?}", run.text, run.ink);
            assert!(!run.bold && run.size == theme::size::MICRO, "bare regular MICRO text");
            assert!((run.at.x - 1834.0).abs() < 0.6, "{} is centred on 1834, got {}", run.text, run.at.x);
        }
        assert_eq!(texts(&rail), runs.iter().map(|run| run.text.clone()).collect::<Vec<_>>());
        // exactly one fill is drawn - the focused band's pill - so there is no position bar, no
        // marker and no track plate beside it
        let fills: Vec<&(u64, Rect)> = shapes.iter().filter(|(tag, _)| *tag != 100).collect();
        assert_eq!(fills.len(), 1, "one pill and nothing else: {shapes:?}");
        let pill = fills[0].1;
        let row = rail.slot(2, At::Drawn);
        assert_eq!((pill.h, pill.w), (38.0, 33.0 + 24.0), "38 high, 12 px of air each side of `85%`");
        assert!((pill.cx() - 1834.0).abs() < 0.01 && (pill.cy() - row.cy()).abs() < 0.01);
        // nothing focused: no pill at all
        let (_, shapes) = drawn(&rig, &rail, None, Some(3));
        assert!(shapes.iter().all(|(tag, _)| *tag == 100), "no fill without a focused band: {shapes:?}");
    }

    #[test]
    fn an_item_among_the_unvalued_marks_no_current_band_so_every_present_band_reads_secondary() {
        let _guard = plx_base::testlock::serial();
        let rig = BandRig::rating(true);
        let rail = rig.rail();
        let current_of = |item| rail.current_for_item(item);
        let primary = |runs: &[plx_ui::draw_census::Run]| runs.iter().filter(|run| run.ink[..3] == theme::TEXT_PRIMARY[..3])
            .map(|run| run.text.clone()).collect::<Vec<_>>();
        let (unrated, _) = drawn(&rig, &rail, None, current_of(37));
        assert!(primary(&unrated).is_empty(), "no band is the current one");
        let (rated, _) = drawn(&rig, &rail, None, current_of(0));
        assert_eq!(primary(&rated), ["95%"]);
    }

    #[test]
    fn a_rail_of_more_bands_than_the_column_holds_scrolls_and_keeps_its_driver_in_view() {
        use super::super::layout::BandRows;
        let rows = BandRows::new(25, &[13]);
        let (_, _, window, max) = rows.geom();
        assert!(max > 0.0 && window < rows.top(25), "25 bands do not fit");
        assert!((0..25).any(|i| (rows.top(i) + 40.0 - window).abs() < 0.01), "the window ends on a row's bottom: {window}");
        for drive in 0..25 {
            for from in [0.0, max, rows.top(drive as isize)] {
                let scroll = rows.scroll_target(from, drive);
                assert!((0.0..=max).contains(&scroll));
                let top = rows.top(drive as isize) - scroll;
                let margin = if drive == 0 || drive == 24 { 0.0 } else { 40.0 };
                assert!(top >= margin - 0.01 && top + 40.0 <= window - margin + 0.01, "band {drive} at {top} in {window} (from {from})");
            }
        }
    }
}
