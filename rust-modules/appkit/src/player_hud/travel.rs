//! **The order vertical focus travel follows: the order the layout draws.** The transport's groups that
//! have a stop in the current state ([`GROUP_SCRUB`], [`GROUP_ROW`], [`GROUP_TABS`], and what an open
//! section draws, [`GROUP_STRIP`] and [`GROUP_SECTION`]), ordered top to bottom by the rects the compiled
//! layout reports ([`HudGeometry`], the open strip's rect, the open section's buttons). Each links to its
//! neighbours both ways ([`Order::links`]); the top group's Up and the bottom group's Down are the
//! screen's edge answers ([`Order::top`], [`Order::bottom`]).
//!
//! It names no package: classic draws its row above the bar and its tabs below, so it comes out as
//! row, bar, tabs; the reference draws the row below the bar and has no tabs, so it comes out as bar, row
//! (and the strip after the row while Chapters is open). A group with no stop in the state is not in the
//! order, and travel skips it (`docs/hud-plugins.md`, D6).
//!
//! One pure function from the compiled state to a value, small enough to build on every frame (a fixed
//! array, no heap): the screen's `links()` and its edge answer both read it.
//!
//! **A modal page is entered and left toward where the tabs are compiled** ([`Order::toward_tabs`]): classic
//! draws its tabs under the page, so Up from a tab enters it and Down off it puts it away; a package whose
//! tabs stand above the page (`rising-tabs`) has them the other way round. The lint asks the same geometry
//! whether that direction exists ([`tabs_span`], [`page_span`]: `HUD-E1501`).

use plx_machine::machine::GroupId;
use plx_ui::screen::{Dir, Link};
use plx_ui::Rect;

use super::geometry::{
    frame_state, geometry, row_len, section_buttons, HudGeometry, GROUP_ROW, GROUP_SCRUB, GROUP_SECTION, GROUP_STRIP,
    GROUP_TABS,
};
use super::command::Section;
use super::state::ControlSlot;
use super::strip::strip_of;
use crate::hud_format::compile::{Compiled, CompiledState};

/// How many groups the transport has to order: one per kind of stop.
const ORDER_SLOTS: usize = 5;

/// **The groups with a stop in a state, top to bottom.** A value: copy it, compare it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Order {
    groups: [GroupId; ORDER_SLOTS],
    len: usize,
}

impl Order {
    const EMPTY: Order = Order { groups: [GROUP_SCRUB; ORDER_SLOTS], len: 0 };

    /// The groups, topmost first.
    pub fn groups(&self) -> &[GroupId] {
        &self.groups[..self.len]
    }

    /// The topmost group: Up off it is the screen's to answer (hiding the transport).
    pub fn top(&self) -> Option<GroupId> {
        self.groups().first().copied()
    }

    /// The bottom group: Down off it stays put.
    pub fn bottom(&self) -> Option<GroupId> {
        self.groups().last().copied()
    }

    /// **The direction from the page to the tabs**: [`Dir::Up`] when the tabs are compiled ABOVE the page an open
    /// section draws (the strip or the section's buttons), [`Dir::Down`] when they are below it, and `Down` when
    /// the state has no tabs or no page, which is the way a modal page has always been left. A modal page is
    /// entered from a tab in [`Order::toward_page`] and left toward the tabs: with classic's tabs under the page,
    /// Up from a tab enters it and Down off the page puts it away; with the tabs above it, the other way round.
    ///
    /// Read off the order, which already sorts the two by their tops; the lint (`HUD-E1501`) is what makes sure the
    /// two never share a height.
    pub fn toward_tabs(&self) -> Dir {
        let at = |want: &[GroupId]| self.groups().iter().position(|g| want.contains(g));
        match (at(&[GROUP_TABS]), at(&[GROUP_STRIP, GROUP_SECTION])) {
            (Some(tabs), Some(page)) if tabs < page => Dir::Up,
            _ => Dir::Down,
        }
    }

    /// **The direction from the tabs to the page**: the opposite of [`Order::toward_tabs`].
    pub fn toward_page(&self) -> Dir {
        match self.toward_tabs() {
            Dir::Up => Dir::Down,
            _ => Dir::Up,
        }
    }

    /// **Each group links to its neighbours**: Down to the one below, Up to the one above. Appends to `out`.
    pub fn links(&self, out: &mut Vec<Link>) {
        for pair in self.groups().windows(2) {
            out.push(Link { from: pair[0], dir: Dir::Down, to: pair[1] });
            out.push(Link { from: pair[1], dir: Dir::Up, to: pair[0] });
        }
    }
}

/// **The order for one frame**, read off the same inputs [`geometry`] and [`row_len`] take: the open
/// sections' bits, the row's occupant, the content and the paused state. A modal package's open section
/// takes the transport's middle (the bar and the row are not on screen, so neither is in the order).
///
/// Presence is read from the report, not from a package: the bar is in the order when the layout places a
/// bar, the row when it has a stop, the tabs when `HudGeometry::tabs` reports a pill (a tab is a pill that opens a
/// section, a disc that opens one is a stop of the row), the strip while Chapters is open and the state has one, and the
/// section group while an open section places buttons. Position is the top of each group's rect, the bar's own track
/// and not the taller grab band; a tie keeps the order above.
pub fn order(compiled: &Compiled, open: u8, slot: ControlSlot, chapters: bool, paused: bool) -> Order {
    let Some((state, skip)) = frame_state(compiled, open, slot, chapters, paused) else {
        return Order::EMPTY;
    };
    let geo = geometry(compiled, open, slot, chapters, paused);
    let state = state.skip_variant(skip);
    let middle = !(open != 0 && compiled.sections_modal());
    let mut stops: [Option<(GroupId, f32)>; ORDER_SLOTS] = [None; ORDER_SLOTS];
    if middle && geo.playbar.w > 0.0 && geo.playbar.h > 0.0 {
        stops[0] = Some((GROUP_SCRUB, geo.playbar.y));
    }
    if middle && row_len(compiled, open, slot, chapters, paused) > 0 {
        stops[1] = Some((GROUP_ROW, geo.row.y));
    }
    // the tab pills (`HudGeometry::tabs` reports pills only: a section a disc opens is a stop of the row)
    stops[2] = tabs_span(&geo).map(|(top, _)| (GROUP_TABS, top));
    if open & Section::Chapters.open_bit() != 0 {
        stops[3] = strip_span(state).map(|(top, _)| (GROUP_STRIP, top));
    }
    if open & Section::Info.open_bit() != 0 {
        stops[4] = buttons_span(state).map(|(top, _)| (GROUP_SECTION, top));
    }
    // insertion sort over at most five entries: stable, so equal tops keep the fixed order above
    let mut order = Order::EMPTY;
    for (group, top) in stops.into_iter().flatten() {
        let mut at = order.len;
        while at > 0 && top_of(&stops, order.groups[at - 1]) > top {
            order.groups[at] = order.groups[at - 1];
            at -= 1;
        }
        order.groups[at] = group;
        order.len += 1;
    }
    order
}

/// A vertical extent: `(top, bottom)`.
type Span = (f32, f32);

fn span_of(rect: Rect) -> Span {
    (rect.y, rect.y + rect.h)
}

fn merged(a: Span, b: Span) -> Span {
    (a.0.min(b.0), a.1.max(b.1))
}

/// **The vertical extent of the tab pills** a geometry report places (`HudGeometry::tabs` reports pills only), `None`
/// with none.
pub(crate) fn tabs_span(geo: &HudGeometry) -> Option<Span> {
    geo.tabs.iter().flatten().map(|pill| span_of(*pill)).reduce(merged)
}

/// The vertical extent of the strip a state places, `None` for a state with no strip.
fn strip_span(state: &CompiledState) -> Option<Span> {
    strip_of(state).map(|strip| span_of(strip.rect))
}

/// The vertical extent of an open section's buttons, `None` for a state that places none.
fn buttons_span(state: &CompiledState) -> Option<Span> {
    section_buttons(state).map(|c| span_of(c.rect)).reduce(merged)
}

/// **The vertical extent of the stops an open section's page places** in `state`, the strip while `open` has Chapters
/// and the section's buttons while it has Info (one section at a time under a modal package), `None` when the page
/// places no stop. The same reads [`order`] takes the page's place in the order from, so the lint's verdict on how
/// the tabs stand against the page ([`tabs_span`]) is the travel order's.
pub(crate) fn page_span(state: &CompiledState, open: u8) -> Option<Span> {
    let strip = (open & Section::Chapters.open_bit() != 0).then(|| strip_span(state)).flatten();
    let buttons = (open & Section::Info.open_bit() != 0).then(|| buttons_span(state)).flatten();
    [strip, buttons].into_iter().flatten().reduce(merged)
}

fn top_of(stops: &[Option<(GroupId, f32)>; ORDER_SLOTS], group: GroupId) -> f32 {
    stops.iter().flatten().find(|(g, _)| *g == group).map_or(f32::MAX, |(_, top)| *top)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hud_format::compile::{Widget, OPEN_CHAPTERS as CHAPTERS, OPEN_INFO as INFO};
    use crate::player_hud::command::HudCommand;
    use crate::player_hud::loaded::{classic_for_test, load};
    use crate::player_hud::state::Controls;
    use plx_ui::fixture::FixtureMeasure;

    fn fixture(text: &str) -> std::sync::Arc<Compiled> {
        load(text.as_bytes(), "0.0.0-test", plx_platform::i18n::english(), &FixtureMeasure).expect("the fixture loads").compiled
    }

    fn reference() -> std::sync::Arc<Compiled> {
        fixture(include_str!("../hud_format/fixtures/reference.plxhud.json"))
    }

    fn order_of(compiled: &Compiled, open: u8) -> Order {
        let slot = ControlSlot::Discs(Controls { previous: false, next: false, restart: false });
        order(compiled, open, slot, true, false)
    }

    fn walk(compiled: &Compiled, open: u8) -> Vec<GroupId> {
        order_of(compiled, open).groups().to_vec()
    }

    fn rising_tabs() -> std::sync::Arc<Compiled> {
        fixture(include_str!("../hud_format/fixtures/rising-tabs.plxhud.json"))
    }

    /// Classic draws its row above the bar and its tabs below: today's row, bar, tabs.
    #[test]
    fn classic_comes_out_as_row_bar_tabs() {
        assert_eq!(walk(&classic_for_test(), 0), [GROUP_ROW, GROUP_SCRUB, GROUP_TABS]);
    }

    /// A modal section takes the middle: the bar and the row leave the order, the tabs stay under the page.
    #[test]
    fn a_modal_section_takes_the_bar_and_the_row_out_of_the_order() {
        let classic = classic_for_test();
        assert_eq!(walk(&classic, CHAPTERS), [GROUP_STRIP, GROUP_TABS]);
        assert_eq!(walk(&classic, INFO), [GROUP_SECTION, GROUP_TABS]);
    }

    /// The reference draws its row below the bar and has no tab pills (its section openers are discs in the row).
    #[test]
    fn the_reference_comes_out_as_bar_row() {
        assert_eq!(walk(&reference(), 0), [GROUP_SCRUB, GROUP_ROW]);
    }

    /// An open strip is drawn under the row and so follows it; Info places no button in the reference, so it has no
    /// stop, and opening it moves nothing in the order.
    #[test]
    fn the_reference_puts_an_open_section_where_it_draws_it() {
        let reference = reference();
        assert_eq!(walk(&reference, CHAPTERS), [GROUP_SCRUB, GROUP_ROW, GROUP_STRIP]);
        assert_eq!(walk(&reference, INFO), [GROUP_SCRUB, GROUP_ROW]);
        assert_eq!(walk(&reference, CHAPTERS | INFO), [GROUP_SCRUB, GROUP_ROW, GROUP_STRIP]);
    }

    /// A package that places buttons in an inline Info section puts that section's group where it draws it: after the
    /// row, and after the strip when both are open (the strip's band is laid out first).
    #[test]
    fn an_inline_info_section_with_buttons_is_in_the_order() {
        let with_actions = crate::player_hud::loaded::reference_with_info_actions_for_test();
        assert_eq!(walk(&with_actions, INFO), [GROUP_SCRUB, GROUP_ROW, GROUP_SECTION]);
        assert_eq!(walk(&with_actions, CHAPTERS | INFO), [GROUP_SCRUB, GROUP_ROW, GROUP_STRIP, GROUP_SECTION]);
    }

    /// `minimal` and `hero-center` draw the row under the bar and have no tabs.
    #[test]
    fn minimal_and_hero_center_come_out_as_bar_row() {
        for text in [
            include_str!("../hud_format/fixtures/minimal.plxhud.json"),
            include_str!("../hud_format/fixtures/hero-center.plxhud.json"),
        ] {
            assert_eq!(walk(&fixture(text), 0), [GROUP_SCRUB, GROUP_ROW]);
        }
    }

    /// `tabs` is an INLINE package whose tab pills share the row's band, under the bar: a tie in the rects keeps the row
    /// before the tabs, and an open strip (drawn lower) follows both.
    #[test]
    fn the_tabs_fixture_keeps_its_pills_after_the_row_it_shares_a_band_with() {
        let tabs = fixture(include_str!("../hud_format/fixtures/tabs.plxhud.json"));
        assert_eq!(walk(&tabs, 0), [GROUP_SCRUB, GROUP_ROW, GROUP_TABS]);
        assert_eq!(walk(&tabs, CHAPTERS), [GROUP_SCRUB, GROUP_ROW, GROUP_TABS, GROUP_STRIP]);
        assert_eq!(walk(&tabs, CHAPTERS | INFO), [GROUP_SCRUB, GROUP_ROW, GROUP_TABS, GROUP_STRIP]);
    }

    /// **A tab is a PILL that opens a section.** A disc that opens one (the reference's) is a stop of the row group, the
    /// one it is pressed as, so `HudGeometry::tabs` does not report it: the report is the one place the pointer's stops,
    /// the tab group and this order all ask, and a disc reported there would be a second stop at the same rect.
    #[test]
    fn a_section_a_disc_opens_has_no_tab() {
        let slot = ControlSlot::Discs(Controls { previous: false, next: false, restart: false });
        let reference = reference();
        for open in [0, CHAPTERS, INFO, CHAPTERS | INFO] {
            let geo = geometry(&reference, open, slot, true, false);
            assert_eq!(geo.tabs, [None, None], "open {open}: the reference's openers are discs of the row");
            let want = Some(HudCommand::ToggleSection(Section::Info));
            let (state, _) = frame_state(&reference, open, slot, true, false).expect("the reference has the state");
            let info = state.controls.iter().find(|c| c.act == want).expect("the Info disc");
            assert_eq!(info.widget, Widget::Disc);
            assert!(geo.items.contains(&Some(info.rect)), "open {open}: the Info disc is an item of the row");
        }
        let tabs = fixture(include_str!("../hud_format/fixtures/tabs.plxhud.json"));
        let geo = geometry(&tabs, 0, slot, true, false);
        assert!(geo.tabs.iter().all(Option::is_some), "a pill is a tab: {:?}", geo.tabs);
    }

    #[test]
    fn each_group_links_to_its_neighbours_both_ways() {
        let mut out = Vec::new();
        let order = order(&classic_for_test(), 0, ControlSlot::Discs(Controls { previous: false, next: false, restart: false }), false, false);
        order.links(&mut out);
        let link = |from, dir, to| Link { from, dir, to };
        assert_eq!(out.len(), 4);
        for want in [
            link(GROUP_ROW, Dir::Down, GROUP_SCRUB),
            link(GROUP_SCRUB, Dir::Up, GROUP_ROW),
            link(GROUP_SCRUB, Dir::Down, GROUP_TABS),
            link(GROUP_TABS, Dir::Up, GROUP_SCRUB),
        ] {
            assert!(out.contains(&want), "{want:?} missing from {out:?}");
        }
        assert_eq!((order.top(), order.bottom()), (Some(GROUP_ROW), Some(GROUP_TABS)));
    }

    /// `rising-tabs` compiles its tabs ABOVE the page an open section draws, so the order has the tabs first.
    #[test]
    fn rising_tabs_puts_its_tabs_above_the_page() {
        let rising = rising_tabs();
        assert_eq!(walk(&rising, CHAPTERS), [GROUP_TABS, GROUP_STRIP]);
        assert_eq!(walk(&rising, INFO), [GROUP_TABS, GROUP_SECTION]);
    }

    /// **The way into and out of a modal page is the way the tabs lie.** Classic's tabs are under the page: toward the
    /// tabs is Down (Down off the page puts it away) and toward the page is Up (Up from a tab enters it). Rising-tabs'
    /// are above it, so both turn round. A state with no tabs, or no page, keeps today's Down.
    #[test]
    fn the_way_into_a_modal_page_follows_where_the_tabs_are_compiled() {
        let classic = classic_for_test();
        let rising = rising_tabs();
        for open in [CHAPTERS, INFO] {
            let below = order_of(&classic, open);
            assert_eq!((below.toward_tabs(), below.toward_page()), (Dir::Down, Dir::Up), "classic, open {open}");
            let above = order_of(&rising, open);
            assert_eq!((above.toward_tabs(), above.toward_page()), (Dir::Up, Dir::Down), "rising-tabs, open {open}");
        }
        // no page open: nothing to enter, today's Down
        for compiled in [&classic, &rising] {
            assert_eq!(order_of(compiled, 0).toward_tabs(), Dir::Down);
        }
        // no tabs in the state (the reference's openers are discs of the row): Down, with the page in the order or not
        let reference = reference();
        for open in [0, CHAPTERS, INFO] {
            assert_eq!(order_of(&reference, open).toward_tabs(), Dir::Down, "reference, open {open}");
        }
        assert_eq!((Order::EMPTY.toward_tabs(), Order::EMPTY.toward_page()), (Dir::Down, Dir::Up));
    }

    /// The lint and the order read one geometry: the page's span is the strip's (Chapters) or the buttons' (Info), the
    /// tabs' is the pills', and rising-tabs' pills stand wholly above the page in both its states, classic's wholly below.
    #[test]
    fn the_spans_the_lint_reads_are_the_orders_and_the_tabs_clear_the_page() {
        let slot = ControlSlot::Discs(Controls { previous: false, next: false, restart: false });
        for (compiled, tabs_above) in [(classic_for_test(), false), (rising_tabs(), true)] {
            for open in [CHAPTERS, INFO] {
                let (state, skip) = frame_state(&compiled, open, slot, true, false).expect("the state");
                let tabs = tabs_span(&state.geometry(skip)).expect("the state places tabs");
                let page = page_span(state, open).expect("the page places stops");
                if tabs_above {
                    assert!(tabs.1 <= page.0, "open {open}: tabs {tabs:?} above page {page:?}");
                } else {
                    assert!(page.1 <= tabs.0, "open {open}: page {page:?} above tabs {tabs:?}");
                }
            }
        }
    }

    /// A package with no state for the frame has nothing to travel between.
    #[test]
    fn a_state_the_package_does_not_have_is_an_empty_order() {
        let classic = classic_for_test();
        assert_eq!(walk(&classic, CHAPTERS | INFO), Vec::<GroupId>::new());
        let mut out = Vec::new();
        Order::EMPTY.links(&mut out);
        assert!(out.is_empty());
        assert_eq!((Order::EMPTY.top(), Order::EMPTY.bottom()), (None, None));
    }
}
