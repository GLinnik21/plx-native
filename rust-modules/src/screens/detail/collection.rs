//! The collection shelf of [`super::DetailScreen`]: a member movie's collection, split out of the
//! Related row. It is headed by the shared linked heading (`ui::linked_heading`), whose own focus
//! group sits above the shelf's cards: UP from any card reaches it, DOWN returns to the card the
//! shelf remembered, OK opens the collection page. The members are ordinary poster cards drawn by
//! the same strip as Related ([`super::related::draw_strip`]).

use crate::metadata::{CollectionShelf, Detail};
use crate::pms::PmsMovie;
use crate::ui::card_row::CardRow;
use crate::ui::linked_heading::LinkedHeading;
use crate::ui::machine::{GroupId, Measure};
use crate::ui::{Painter, Rect};

use super::related;

pub(crate) const COLLECTION_ELEM_RANGE_START: u32 = 1760;
/// The heading's element. It names a slot, not a server item, so it needs no interned identity:
/// it is the same key whichever collection the page is showing.
pub(crate) const HEADING_ELEM: u32 = COLLECTION_ELEM_RANGE_START;
const MEMBERS_START: u32 = COLLECTION_ELEM_RANGE_START + 1;
pub(crate) const COLLECTION_ELEM_RANGE_END: u32 = 1792;
pub(crate) const COLLECTION_GROUP: GroupId = GroupId(7);
pub(crate) const HEADING_GROUP: GroupId = GroupId(8);

const _: () = assert!(
    (COLLECTION_ELEM_RANGE_END - MEMBERS_START) as usize >= crate::metadata::COLLECTION_MAX
);

pub(crate) fn elem(index: usize) -> Option<u32> {
    (index < (COLLECTION_ELEM_RANGE_END - MEMBERS_START) as usize)
        .then_some(MEMBERS_START + index as u32)
}

pub(crate) fn locate(key: u32) -> Option<usize> {
    (MEMBERS_START..COLLECTION_ELEM_RANGE_END)
        .contains(&key)
        .then(|| (key - MEMBERS_START) as usize)
}

pub(crate) fn members(d: &Detail) -> &[PmsMovie] {
    d.collection.as_ref().map_or(&[], |c| c.members.as_slice())
}

pub(crate) fn len(d: &Detail) -> usize {
    members(d).len()
}

pub(crate) fn item<'a>(d: &'a Detail, key: u32) -> Option<&'a PmsMovie> {
    members(d).get(locate(key)?)
}

/// OK on a member card: that member's Detail, exactly as a Related card opens one.
pub(crate) fn action(d: &Detail, key: u32) -> related::Action {
    match item(d, key) {
        Some(m) if !m.rk.is_empty() => related::Action::OpenDetail(m.sid, m.rk.clone()),
        _ => related::Action::None,
    }
}

/// Where OK on the heading goes: the collection page, by section and tag — `/related` names the
/// collection by its TAG id, and the collection store resolves the rating key from it.
pub(crate) fn target(d: &Detail) -> Option<crate::screens::registry::ContentArg> {
    let c = d.collection.as_ref()?;
    Some(crate::screens::registry::ContentArg::Collection(c.link(d.sid)))
}

pub(crate) fn heading(c: &CollectionShelf) -> LinkedHeading<'_> {
    LinkedHeading::heading(&c.title, "")
}

/// The heading's face at `top` (the section's top on the caller's scroll basis), `lift` being the
/// row's live label lift — 0 on a settled basis.
pub(crate) fn heading_rect(
    c: &CollectionShelf,
    top: f32,
    lift: f32,
    focused: bool,
    measure: &dyn Measure,
) -> Rect {
    let h = heading(c);
    let m = h.measure(measure);
    h.face_rect(crate::ui::consts::MARGIN_X, top - lift, f32::from(focused), &m)
}

pub(crate) fn rect(row: &CardRow, index: usize, top: f32, at_drawn: bool) -> Rect {
    related::rect(row, index, top, at_drawn)
}

pub(crate) fn block_h(band: f32) -> f32 {
    related::block_h(band)
}

pub(crate) fn draw(
    p: Painter,
    d: &Detail,
    row: &CardRow,
    top: f32,
    focused: Option<usize>,
    heading_focused: bool,
    measure: &dyn Measure,
) {
    let Some(c) = d.collection.as_ref() else { return };
    let h = heading(c);
    let m = h.measure(measure);
    h.draw(
        p,
        crate::ui::consts::MARGIN_X,
        top - row.lift(),
        f32::from(heading_focused),
        &m,
        measure,
    );
    related::draw_strip(p, &c.members, row, top, focused, measure);
}

pub(crate) fn draw_focused(
    p: Painter,
    d: &Detail,
    row: &CardRow,
    index: usize,
    top: f32,
    press: f32,
    measure: &dyn Measure,
) {
    related::draw_focused_in(p, members(d), row, index, top, press, measure);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_member_key_round_trips_and_never_names_the_heading() {
        for i in 0..crate::metadata::COLLECTION_MAX {
            assert_eq!(locate(elem(i).unwrap()), Some(i));
        }
        assert_eq!(locate(HEADING_ELEM), None);
        assert_eq!(elem((COLLECTION_ELEM_RANGE_END - MEMBERS_START) as usize), None);
    }
}
