//! **The column every page that wears the tab bar is: NAV, HEADER, CONTENT, RAIL.** The Library,
//! its Collections and Recommended pages and the Watchlist each used to declare how focus travels
//! between those regions by hand, and the four declarations disagreed: one page's Up from the
//! header reached the bar and another's found nothing, BACK was decided three ways, and one of the
//! three read the bar as "nothing on this page has focus" and looped between the header and the
//! bar for ever. This is the one declaration they read instead.
//!
//! It is a TABLE over the focus engine, not a second engine. [`PageRegions::links`] writes the
//! `Link`s the engine already takes (`FocusEngine::move_dir` still does every move), and
//! [`PageRegions::back`] answers the one question the engine has no word for, what BACK does from
//! where focus is. The page keeps what only it knows: which of its groups are the header and the
//! content, how to leave, how to scroll back to its top.
//!
//! The regions, top to bottom of the screen:
//!
//! | Region | What it is | Who moves focus in it |
//! |---|---|---|
//! | [`Region::Nav`] | the shared bar (`dispatch::STRIP`) | the dispatcher; no page code |
//! | [`Region::Header`] | one row of stops under the bar | [`row_neighbour`], Left/Right, no wrap |
//! | [`Region::Content`] | the page's column of groups, top to bottom | the engine, over [`PageRegions::links`] |
//! | [`Region::Rail`] | the A-Z rail beside a grid | `MasterDetail`; horizontal only |
//!
//! **Travel is the same on every page.** Up from the head of the content seats the header; Up from
//! the header seats the page's OWN pill on the bar (`TabContainer::strip_selected`, one rule in the
//! dispatcher's strip seat); Down from the bar seats the header, and from the header the content.
//! Left and Right on the header stop at its ends. [`PageRegions::links`] is where that column is
//! written, so a page that declares a `Link` involving `STRIP`, its header group or the head of its
//! content by hand has a second declaration of the column.
//!
//! **BACK is one function** ([`PageRegions::back`]). Deep in the content it returns to the top
//! first; from the top it either leaves the page or walks up the column one region at a time,
//! which is the page's `back_walks_header` and is true only on the Library (header, then bar, then
//! Home). A strip key carries the PAGE's own entry, so whether focus is "on the bar" is
//! `elem >= STRIP_BASE` and never `key.entry`; [`PageRegions::region_of`] is where that is decided.
//!
//! **The header row is one component** ([`crate::page_header::HeaderRow`] and its borrowed
//! [`crate::page_header::HeaderFocus`]): the row's layout, paint, focus group and Left/Right
//! ([`row_neighbour`] over its stop list) are the same on every page that wears one, and its motion
//! (the focus capsule that travels, the selection plate that lands) is held by the SCREEN, stepped
//! from the engine's current focus, so rebuilding the row for a new library moves nothing on screen.
//!
//! The module lives in `plx_ui` because both the screens and the application name it, and it
//! names only `screen` types, `GroupId` and `dispatch::STRIP_BASE`.
use crate::containers::tabs::STRIP;
use crate::dispatch::STRIP_BASE;
use crate::screen::{Dir, Link, Step};
use plx_machine::machine::{EntryId, FocusKey, GroupId};

/// Where focus is on a page, in the column's terms.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Region {
    /// The shared bar's pills.
    Nav,
    /// The row of stops under the bar.
    Header,
    /// The page's column: status, grid, shelves and the headings linked to them.
    Content,
    /// The A-Z rail beside the grid.
    Rail,
}

/// Which region a page seats the first time it is entered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arrival {
    Header,
    Content,
}

/// What one BACK does, as a step for the page to execute.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackStep {
    /// From the rail to the grid.
    Content,
    /// Deep in the content: scroll to the top and seat the first row in the same column.
    Top,
    /// Seat the header.
    Header,
    /// Seat the bar (on the page's own pill).
    Nav,
    /// Leave the page, by whatever request the page leaves with.
    Leave,
}

/// A page's regions, declared once and read by the engine's links, the page's BACK and its first
/// seat. Built per call from the page's current state (a page's content is whatever groups it holds
/// now), so it holds no state of its own.
#[derive(Clone, Copy, Debug)]
pub struct PageRegions<'a> {
    /// The header row's group. `None` when the header has no stop to land on: the content's head
    /// then links straight to the bar.
    pub header: Option<GroupId>,
    /// The content's groups, top to bottom (a linked heading is its own group above its shelf).
    pub content: &'a [GroupId],
    /// The A-Z rail's group, on a page that has one.
    pub rail: Option<GroupId>,
    /// Does BACK from the top of the content walk up through the header and the bar before it
    /// leaves? The Library's header-then-bar-then-Home walk is the only one the design asks for.
    pub back_walks_header: bool,
    /// The region the first arrival seats.
    pub arrival: Arrival,
}

impl PageRegions<'_> {
    /// **The page's column as `Link`s**: the bar and the header, the header and the head of the
    /// content, and each content group and the one below it. With no header the bar links to the
    /// head of the content directly. A page with neither declares no links here.
    ///
    /// The engine takes the FIRST link that matches (`FocusEngine::move_dir`), so a page calls this
    /// first and appends only the links that are its own (the rail's, a linked heading's).
    pub fn links(&self, out: &mut Vec<Link>) {
        let column: Vec<GroupId> = self.header.into_iter().chain(self.content.iter().copied()).collect();
        let Some(&head) = column.first() else { return };
        out.push(Link { from: STRIP, dir: Dir::Down, to: head });
        out.push(Link { from: head, dir: Dir::Up, to: STRIP });
        for pair in column.windows(2) {
            out.push(Link { from: pair[0], dir: Dir::Down, to: pair[1] });
            out.push(Link { from: pair[1], dir: Dir::Up, to: pair[0] });
        }
    }

    /// The region `elem` is in, given the group the page puts it in. A strip key carries the page's
    /// own entry, so the bar is recognised by the element alone: `elem >= STRIP_BASE`. `None` for an
    /// element of none of the declared groups (a popover's, a stale key).
    pub fn region_of(&self, elem: u32, group: Option<GroupId>) -> Option<Region> {
        if elem >= STRIP_BASE {
            return Some(Region::Nav);
        }
        let group = group?;
        if self.header == Some(group) {
            Some(Region::Header)
        } else if self.rail == Some(group) {
            Some(Region::Rail)
        } else if self.content.contains(&group) {
            Some(Region::Content)
        } else {
            None
        }
    }

    /// The group the first arrival seats: the header or the head of the content by the page's
    /// [`Arrival`], whichever exists when the preferred one does not (the Watchlist before its
    /// first title has landed seats its header).
    pub fn first_seat(&self) -> Option<GroupId> {
        match self.arrival {
            Arrival::Header => self.header.or_else(|| self.content.first().copied()),
            Arrival::Content => self.content.first().copied().or(self.header),
        }
    }

    /// **What BACK does** with focus in `at` (`None`: nothing of this page holds focus) and the
    /// content `scrolled` away from its top.
    ///
    /// | Focus is in | Step |
    /// |---|---|
    /// | the rail | [`BackStep::Content`] |
    /// | the content, scrolled | [`BackStep::Top`] |
    /// | the content at its top | [`BackStep::Header`] when the page walks the header, else [`BackStep::Leave`] |
    /// | the header | [`BackStep::Nav`] when the page walks the header, else [`BackStep::Leave`] |
    /// | the bar, or nowhere | [`BackStep::Leave`] |
    pub fn back(&self, at: Option<Region>, scrolled: bool) -> BackStep {
        match at {
            None | Some(Region::Nav) => BackStep::Leave,
            Some(Region::Rail) => BackStep::Content,
            Some(Region::Content) if scrolled => BackStep::Top,
            Some(Region::Content) if !self.back_walks_header => BackStep::Leave,
            // No header stop to land on: the next region up is the bar.
            Some(Region::Content) => if self.header.is_some() { BackStep::Header } else { BackStep::Nav },
            Some(Region::Header) => if self.back_walks_header { BackStep::Nav } else { BackStep::Leave },
        }
    }
}

/// **A Left or Right step along a row of stops**, no wrap: the neighbour a row-shaped group answers
/// with, for the header and for a shelf's cards alike. `Step::Edge` at either end and for Up or
/// Down, which leaves the engine to the group's `Link`s and edge rule (the header's edges Stop
/// Left and Right, so the row never lets focus out sideways).
pub fn row_neighbour(entry: EntryId, stops: &[u32], key: FocusKey<u32>, dir: Dir) -> Step<u32> {
    let Some(index) = stops.iter().position(|stop| *stop == key.elem) else { return Step::Edge };
    let next = match dir {
        Dir::Left => index.checked_sub(1),
        Dir::Right => Some(index + 1),
        Dir::Up | Dir::Down => None,
    };
    next.and_then(|i| stops.get(i).copied())
        .map_or(Step::Edge, |elem| Step::Move(FocusKey { entry, elem }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: GroupId = GroupId(1);
    const GRID: GroupId = GroupId(2);
    const STATUS: GroupId = GroupId(3);
    const RAIL: GroupId = GroupId(4);
    const E: EntryId = EntryId(9);

    fn regions<'a>(content: &'a [GroupId], walks: bool, arrival: Arrival) -> PageRegions<'a> {
        PageRegions { header: Some(HEADER), content, rail: Some(RAIL), back_walks_header: walks, arrival }
    }

    fn link(from: GroupId, dir: Dir, to: GroupId) -> Link { Link { from, dir, to } }

    #[test]
    fn the_column_is_the_bar_the_header_and_the_content_top_to_bottom() {
        let mut out = Vec::new();
        regions(&[STATUS, GRID], false, Arrival::Content).links(&mut out);
        assert_eq!(out, [
            link(STRIP, Dir::Down, HEADER), link(HEADER, Dir::Up, STRIP),
            link(HEADER, Dir::Down, STATUS), link(STATUS, Dir::Up, HEADER),
            link(STATUS, Dir::Down, GRID), link(GRID, Dir::Up, STATUS),
        ]);
    }

    #[test]
    fn with_no_header_the_bar_links_to_the_head_of_the_content() {
        let mut out = Vec::new();
        PageRegions { header: None, ..regions(&[GRID], false, Arrival::Content) }.links(&mut out);
        assert_eq!(out, [link(STRIP, Dir::Down, GRID), link(GRID, Dir::Up, STRIP)]);
        let mut none = Vec::new();
        PageRegions { header: None, ..regions(&[], false, Arrival::Content) }.links(&mut none);
        assert!(none.is_empty(), "a page with no groups declares no links");
    }

    #[test]
    fn the_bar_is_the_element_never_the_entry() {
        let r = regions(&[GRID], true, Arrival::Header);
        // A strip key carries the page's entry, and group_of knows nothing of it.
        assert_eq!(r.region_of(STRIP_BASE, None), Some(Region::Nav));
        assert_eq!(r.region_of(STRIP_BASE + 4, Some(HEADER)), Some(Region::Nav));
        assert_eq!(r.region_of(7, Some(HEADER)), Some(Region::Header));
        assert_eq!(r.region_of(7, Some(GRID)), Some(Region::Content));
        assert_eq!(r.region_of(7, Some(RAIL)), Some(Region::Rail));
        assert_eq!(r.region_of(7, Some(GroupId(99))), None);
        assert_eq!(r.region_of(7, None), None);
    }

    #[test]
    fn the_first_seat_is_the_arrival_region_or_whichever_exists() {
        assert_eq!(regions(&[GRID], false, Arrival::Header).first_seat(), Some(HEADER));
        assert_eq!(regions(&[GRID], false, Arrival::Content).first_seat(), Some(GRID));
        // The Watchlist before a title has landed: no content yet, so the header holds the seat.
        assert_eq!(regions(&[], false, Arrival::Content).first_seat(), Some(HEADER));
        assert_eq!(PageRegions { header: None, ..regions(&[GRID], false, Arrival::Header) }.first_seat(), Some(GRID));
        assert_eq!(PageRegions { header: None, ..regions(&[], false, Arrival::Header) }.first_seat(), None);
    }

    #[test]
    fn back_walks_up_the_column_only_on_a_page_that_walks_the_header() {
        let walks = regions(&[GRID], true, Arrival::Header);
        let leaves = regions(&[GRID], false, Arrival::Content);
        for (at, scrolled, walk, leave) in [
            (Some(Region::Content), true, BackStep::Top, BackStep::Top),
            (Some(Region::Content), false, BackStep::Header, BackStep::Leave),
            (Some(Region::Header), false, BackStep::Nav, BackStep::Leave),
            (Some(Region::Header), true, BackStep::Nav, BackStep::Leave),
            (Some(Region::Nav), false, BackStep::Leave, BackStep::Leave),
            (Some(Region::Nav), true, BackStep::Leave, BackStep::Leave),
            (Some(Region::Rail), true, BackStep::Content, BackStep::Content),
            (Some(Region::Rail), false, BackStep::Content, BackStep::Content),
            (None, false, BackStep::Leave, BackStep::Leave),
        ] {
            assert_eq!(walks.back(at, scrolled), walk, "walking page, {at:?}, scrolled {scrolled}");
            assert_eq!(leaves.back(at, scrolled), leave, "leaving page, {at:?}, scrolled {scrolled}");
        }
        // The walk terminates: every step up the column is one region higher, and the bar leaves.
        let mut at = Some(Region::Content);
        let mut steps = Vec::new();
        for _ in 0..5 {
            let step = walks.back(at, false);
            steps.push(step);
            at = match step {
                BackStep::Header => Some(Region::Header),
                BackStep::Nav => Some(Region::Nav),
                _ => break,
            };
        }
        assert_eq!(steps, [BackStep::Header, BackStep::Nav, BackStep::Leave]);
    }

    #[test]
    fn a_walking_page_with_no_header_stop_goes_straight_to_the_bar() {
        let r = PageRegions { header: None, ..regions(&[GRID], true, Arrival::Content) };
        assert_eq!(r.back(Some(Region::Content), false), BackStep::Nav);
    }

    #[test]
    fn a_row_steps_left_and_right_and_stops_at_both_ends() {
        let stops = [10, 11, 12];
        let at = |elem, dir| row_neighbour(E, &stops, FocusKey { entry: E, elem }, dir);
        assert!(matches!(at(11, Dir::Right), Step::Move(key) if key == FocusKey { entry: E, elem: 12 }));
        assert!(matches!(at(11, Dir::Left), Step::Move(key) if key == FocusKey { entry: E, elem: 10 }));
        assert!(matches!(at(10, Dir::Left), Step::Edge), "no wrap at the first stop");
        assert!(matches!(at(12, Dir::Right), Step::Edge), "no wrap at the last stop");
        assert!(matches!(at(11, Dir::Up), Step::Edge) && matches!(at(11, Dir::Down), Step::Edge));
        assert!(matches!(at(99, Dir::Right), Step::Edge), "a key that is not on the row");
    }
}
