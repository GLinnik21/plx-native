//! **The travel rule: the tabs and an open modal page never share a height** (`docs/hud-plugins.md`, D6; the
//! direction itself is [`Order::toward_tabs`](crate::player_hud::travel::Order::toward_tabs)).
//!
//! A modal package's open section takes the middle of the transport and is entered from the tab that opened
//! it and left toward the tabs: Up from a tab enters a page that lies above its tabs and Down off it puts it
//! away (classic), the other way round for a page that lies below them (`rising-tabs`). That direction is read
//! off the order the layout is drawn in, so it exists only if the tabs lie WHOLLY above or WHOLLY below the
//! page's stops. If their vertical extents overlap (tabs in a band beside the strip, or between an Info page's
//! buttons) the way in has no direction, and the package is `HUD-E1501`.
//!
//! **What is checked.** Every compiled state of a package with `sections: "modal"` that has a section open,
//! in every language, valuation and slot the compile has it in. Skipped, because there is no way in to fail:
//! an inline package (its page is in the order with the rest and travel walks it freely), a state whose page
//! places no stops (Info with no buttons, so the tabs have nothing to lead to), and a state that places no
//! tab pill (a section a disc of the row opens has no tab).
//!
//! **One geometry, the travel's.** The page's extent and the tabs' are [`page_span`] and [`tabs_span`], the very
//! reads [`order`](crate::player_hud::travel::order) takes the position of each group from (`strip_of`,
//! `section_buttons` and `HudGeometry::tabs`), so the lint's verdict and the travel's direction cannot be two
//! answers. Two extents that only touch (`tab.bottom == page.top`) are apart: the overlap is measured past
//! `EDGE_TOL`.
//!
//! **A finding is a tab.** The pointer is the pill's item, so the author moves the tabs rather than the page; a
//! package with several pills in the page's band reports each (the first state it overlaps in), and one whose
//! pills stand on both sides of the page, none overlapping it alone, reports the first pill.

use plx_ui::consts::{EDGE_TOL, SCR_W};
use plx_ui::Rect;

use super::compile::{SkipKind, Widget};
use super::lint::{ctx_for, named, LangCtx, StateCtx};
use super::{codes, Finding};
use crate::player_hud::travel::{page_span, tabs_span};
#[cfg(test)]
use super::lint::testing::{Base, InvalidCase};

/// A vertical extent: `(top, bottom)`.
type Span = (f32, f32);

/// Report this module's findings for the language of `cx`.
pub(super) fn check(cx: &LangCtx<'_>, out: &mut Vec<Finding>) {
    if !cx.compiled.sections_modal() {
        return;
    }
    let mut named_already: Vec<String> = Vec::new();
    for state in cx.states().filter(|s| s.key.open != 0) {
        let Some((tabs, page)) = spans(&state) else { continue };
        if !overlaps(tabs, page) {
            continue;
        }
        for (pointer, pill) in overlapping_pills(&state, page) {
            if named_already.iter().any(|p| *p == pointer) {
                continue;
            }
            out.push(finding(cx, &state, pointer, pill, page));
            named_already.push(pointer.to_owned());
        }
    }
}

/// The tabs' extent and the page's in `state`, `None` when the state is skipped: it places no tab pill, or its
/// page places no stop.
fn spans(state: &StateCtx<'_>) -> Option<(Span, Span)> {
    let tabs = tabs_span(&state.state.geometry(SkipKind::Intro))?;
    let page = page_span(state.state, state.key.open)?;
    Some((tabs, page))
}

/// Do two vertical extents share a height? Touching ones do not.
fn overlaps(a: Span, b: Span) -> bool {
    a.0 < b.1 - EDGE_TOL && b.0 < a.1 - EDGE_TOL
}

/// The tab pills of `state` that share a height with `page`, each with its item's pointer and its rect, in the
/// order the state places them. A tab is a PILL of a `toggle_section` control (`HudGeometry::tabs`); when the
/// group overlaps but no pill does alone, the first pill stands for it.
fn overlapping_pills<'a>(state: &'a StateCtx<'_>, page: Span) -> Vec<(&'a str, Rect)> {
    let pills: Vec<(&str, Rect)> = state
        .state
        .controls
        .iter()
        .enumerate()
        .filter(|(_, c)| c.widget == Widget::Pill && c.command == "toggle_section")
        .map(|(i, c)| (state.state.control_pointer(i), c.rect))
        .collect();
    let hit: Vec<(&str, Rect)> = pills.iter().copied().filter(|(_, r)| overlaps((r.y, r.y + r.h), page)).collect();
    if hit.is_empty() { pills.into_iter().take(1).collect() } else { hit }
}

fn finding(cx: &LangCtx<'_>, state: &StateCtx<'_>, pointer: &str, pill: Rect, page: Span) -> Finding {
    let mut ctx = ctx_for(cx, state);
    ctx.rects.push(named("tab", pill));
    ctx.rects.push(named("page", Rect::new(0.0, page.0, SCR_W, page.1 - page.0)));
    Finding::new(
        &codes::E1501,
        pointer,
        format!(
            "this tab (y {top} to {bottom}) shares a height with the stops of the page it opens (y {page_top} to {page_bottom}): \
             with a modal section open the way into the page is the direction it lies from the tabs, and these two have no direction",
            top = pill.y,
            bottom = pill.y + pill.h,
            page_top = page.0,
            page_bottom = page.1,
        ),
        "lay the tabs out in a band of their own, wholly above or wholly below the section's strip or buttons, as classic \
         (below) and rising-tabs (above) do",
    )
    .with(ctx)
}

/// The invalid packages of this module's codes (`lint::testing::assert_case` runs each): the base, the
/// exact-once edits, the code and pointer the edit must yield.
#[cfg(test)]
pub(super) const INVALID: &[InvalidCase] = &[
    // rising-tabs whose Chapters state lays a second pair of tabs out beside the strip, in the Chapters band, in place
    // of the band of their own (which now draws everywhere but there): the pills stand at the height of the strip's
    // cards, so the way into the page has no direction. The Info state keeps its tabs above its buttons and is clean
    InvalidCase {
        name: "rising-tabs with its Chapters tabs beside the strip: they share a height with its cards",
        code: "HUD-E1501",
        pointer: "/layout/block/4/cells/0/segment/items/0",
        base: Base::RisingTabs,
        edits: TABS_BESIDE_THE_STRIP,
        langs: &[],
        skew: 0.0,
        also: &[("HUD-E1501", "/layout/block/4/cells/0/segment/items/1")],
    },
];

/// `(code, reason)` for a code of this module that no parseable package can reach. `HUD-E1501` has an invalid case.
#[cfg(test)]
pub(super) const UNIT_ONLY: &[(&str, &str)] = &[];

/// The edits of the invalid case: the `tabs` band stops drawing in the Chapters state, and the Chapters band
/// starts with the tab segment, its own pills named apart (an id is unique in the package).
#[cfg(test)]
pub(super) const TABS_BESIDE_THE_STRIP: &[(&str, &str)] = &[
    (
        "        \"id\": \"tabs\",\n        \"gap_before\": \"SM\",\n",
        "        \"id\": \"tabs\",\n        \"when\": \"!panel.chapters\",\n        \"gap_before\": \"SM\",\n",
    ),
    (
        concat!(
            "        \"id\": \"chapters\",\n",
            "        \"when\": \"panel.chapters\",\n",
            "        \"gap_before\": \"SM\",\n",
            "        \"cells\": [\n",
        ),
        concat!(
            "        \"id\": \"chapters\",\n",
            "        \"when\": \"panel.chapters\",\n",
            "        \"gap_before\": \"SM\",\n",
            "        \"cells\": [\n",
            "          { \"align\": \"start\", \"segment\": { \"id\": \"side_tabs\", \"gap\": \"SM\", \"items\": [\n",
            "            { \"id\": \"side_info\", \"command\": \"toggle_section\", \"section\": \"info\", \"widget\": \"pill\", \"size\": 52 },\n",
            "            { \"id\": \"side_chapters\", \"command\": \"toggle_section\", \"section\": \"chapters\", \"widget\": \"pill\", \"size\": 52, \"when\": \"chapters.present\" }\n",
            "          ] } },\n",
        ),
    ),
];

/// rising-tabs' Info band with its buttons taken out: the page places no stop, so nothing leads into it from the tabs.
#[cfg(test)]
const WITHOUT_THE_BUTTONS: &[(&str, &str)] = &[(
    concat!(
        "          },\n",
        "          {\n",
        "            \"align\": \"end\",\n",
        "            \"valign\": \"center\",\n",
        "            \"actions\": {\n",
        "              \"axis\": \"column\",\n",
        "              \"gap\": 16,\n",
        "              \"item_h\": 70,\n",
        "              \"min_w\": 352,\n",
        "              \"items\": [\n",
        "                {\n",
        "                  \"id\": \"from_beginning\",\n",
        "                  \"command\": \"from_beginning\",\n",
        "                  \"widget\": \"button\",\n",
        "                  \"size\": 70\n",
        "                },\n",
        "                {\n",
        "                  \"id\": \"go_to_detail\",\n",
        "                  \"command\": \"go_to_detail\",\n",
        "                  \"widget\": \"button\",\n",
        "                  \"size\": 70\n",
        "                }\n",
        "              ]\n",
        "            }\n",
        "          }\n",
        "        ]\n",
    ),
    "          }\n        ]\n",
)];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hud_format::compile::{compile, CompileCtx, OPEN_CHAPTERS, OPEN_INFO};
    use crate::hud_format::lint::testing::apply;
    use crate::hud_format::lint::{lint, LintLang, LintOptions};
    use crate::hud_format::parse;
    use plx_base::fontcov::advances::ShippedMeasure;

    const RISING_TABS: &str = include_str!("fixtures/rising-tabs.plxhud.json");

    fn in_en<R>(text: &str, f: impl FnOnce(&LangCtx<'_>) -> R) -> R {
        let lang = LintLang::for_tag("en").expect("English");
        let package = parse::parse(text.as_bytes()).package.expect("a package");
        let compiled = compile(&package, &ShippedMeasure, CompileCtx { labels: lang.labels.clone() });
        let opts = LintOptions::default();
        f(&LangCtx { package: &package, compiled: &compiled, measure: &ShippedMeasure, lang: &lang, opts: &opts })
    }

    /// This module's findings alone, in English.
    fn found(text: &str) -> Vec<Finding> {
        in_en(text, |cx| {
            let mut out = Vec::new();
            check(cx, &mut out);
            out
        })
    }

    /// `edits` applied to rising-tabs.
    fn edited(edits: &[(&str, &str)]) -> String {
        apply(Base::RisingTabs, edits)
    }

    /// Two extents are apart when they only touch, and share a height when they do not.
    #[test]
    fn extents_that_touch_are_apart_and_extents_that_cross_overlap() {
        assert!(!overlaps((0.0, 10.0), (10.0, 20.0)), "tabs above the page, touching it");
        assert!(!overlaps((10.0, 20.0), (0.0, 10.0)), "tabs below the page, touching it");
        assert!(!overlaps((0.0, 10.0), (11.0, 20.0)));
        assert!(overlaps((0.0, 10.5), (10.0, 20.0)), "half a pixel in");
        assert!(overlaps((0.0, 30.0), (10.0, 20.0)), "tabs around the page");
        assert!(overlaps((12.0, 14.0), (10.0, 20.0)), "tabs inside the page");
    }

    /// The shipped modal packages that place tabs lay them wholly below (classic) or above (rising-tabs) the page
    /// in every state with a section open, and the walk is not vacuous: each has such a state.
    #[test]
    fn tabs_above_or_below_the_page_are_clean() {
        for text in [crate::hud_format::BUILTIN_CLASSIC, RISING_TABS] {
            assert_eq!(found(text), Vec::<Finding>::new());
            let checked = in_en(text, |cx| cx.states().filter(|s| s.key.open != 0 && spans(s).is_some()).count());
            assert!(checked > 0, "a state with a section open, tabs and a page");
        }
    }

    /// **An inline package is not checked**: its page is in the order with the rest and travel walks it freely. The
    /// invalid case declared `inline` shares the same heights (the premise, asserted) and finds nothing.
    #[test]
    fn an_inline_package_is_not_checked() {
        let modal = edited(TABS_BESIDE_THE_STRIP);
        let inline = modal.replace("\"sections\": \"modal\"", "\"sections\": \"inline\"");
        assert_ne!(inline, modal, "the edit applied");
        in_en(&inline, |cx| {
            assert!(!cx.compiled.sections_modal());
            let shared = cx.states().filter(|s| s.key.open != 0).filter_map(|s| spans(&s)).any(|(tabs, page)| overlaps(tabs, page));
            assert!(shared, "the premise: the tabs share a height with the page");
        });
        assert_eq!(found(&inline), Vec::<Finding>::new());
        assert!(!found(&modal).is_empty(), "…and the modal one is found");
    }

    /// **A state whose page places no stops has no way in to fail.** rising-tabs' Info band without its buttons: the
    /// Info state still places its tabs but its page has no stop, so it is skipped; the Chapters state is checked.
    #[test]
    fn a_page_with_no_stops_is_skipped() {
        let bare = edited(WITHOUT_THE_BUTTONS);
        in_en(&bare, |cx| {
            let info = cx.states().find(|s| s.key.open == OPEN_INFO).expect("an Info state");
            assert!(tabs_span(&info.state.geometry(SkipKind::Intro)).is_some(), "the Info state places tabs");
            assert!(page_span(info.state, OPEN_INFO).is_none(), "…and its page places no stop");
            assert!(spans(&info).is_none(), "so it is skipped");
            let chapters = cx.states().find(|s| s.key.open == OPEN_CHAPTERS).expect("a Chapters state");
            assert!(spans(&chapters).is_some(), "the Chapters state has tabs and a strip: it is checked");
        });
        assert_eq!(found(&bare), Vec::<Finding>::new());
    }

    /// A state that places no tab pill has nothing to check either: the compact state of a package whose page opens
    /// by no tab, and the reference, whose openers are discs of the row.
    #[test]
    fn a_state_with_no_tabs_is_skipped() {
        in_en(include_str!("fixtures/reference.plxhud.json"), |cx| {
            for state in cx.states().filter(|s| s.key.open != 0) {
                assert!(tabs_span(&state.state.geometry(SkipKind::Intro)).is_none(), "the reference places no tab pill");
                assert!(spans(&state).is_none());
            }
        });
    }

    /// The finding names the tab, the state and slot it was found in and carries the pill's and the page's rects,
    /// once per tab however many valuations and slots reach the state.
    #[test]
    fn the_finding_names_the_tab_and_carries_the_two_extents() {
        let text = edited(TABS_BESIDE_THE_STRIP);
        let all = lint(&text, &ShippedMeasure, &[LintLang::for_tag("en").expect("English")]);
        let ours: Vec<&Finding> = all.iter().filter(|f| f.code == "HUD-E1501").collect();
        let pointers: Vec<&str> = ours.iter().map(|f| f.pointer.as_str()).collect();
        assert_eq!(
            pointers,
            ["/layout/block/4/cells/0/segment/items/0", "/layout/block/4/cells/0/segment/items/1"],
            "one finding per overlapping tab, not one per state or valuation"
        );
        let first = ours[0];
        assert!(first.ctx.state.as_deref().is_some_and(|s| s.starts_with("chapters")), "{:?}", first.ctx.state);
        assert_eq!(first.ctx.slot.as_deref(), Some("discs"));
        let names: Vec<&str> = first.ctx.rects.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["tab", "page"]);
        let (tab, page) = (&first.ctx.rects[0], &first.ctx.rects[1]);
        assert!(tab.y < page.y + page.h && page.y < tab.y + tab.h, "the two rects share a height: {tab:?} {page:?}");
        assert!(first.message.contains("shares a height") && !first.hint.is_empty());
    }
}
