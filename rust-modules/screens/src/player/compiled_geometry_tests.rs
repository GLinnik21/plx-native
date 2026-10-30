//! **The player page places, hit-tests and lifts captions from the COMPILED package's geometry**, not from
//! classic's constants: `Focusable::place`, the row group's extent and length, the scrubber's pointer
//! mapping and the caption band all read the report `player_hud::geometry` builds from the package the
//! app handed in (`PlayerScreen::compiled`).
//!
//! Each case builds a package that differs from `builtin:classic` where the consumer reads (the bar a
//! row higher and pushed in from the left, a sixth control in the row) and holds the page to the
//! package's numbers. They were watched RED first with the consumers still on `classic::*` (the same
//! page, `compiled` set, answered classic's rects), then green once they read the compile.
//!
//! The existing pins of the page that build `PlayerScreen::new(ENTRY)` leave `compiled` at `None`, which is
//! the stage bridge: those keep grading classic's own constants (stage 3 retires them with `draw_hud`).

use std::sync::Arc;

use super::*;
use crate::player::overlay_tests::TestHost;
use plx_appkit::hud_format::compile::{Compiled, SkipKind};
use plx_appkit::hud_format::reach::Slot;
use plx_appkit::hud_format::CLASSIC_PACKAGE;
use plx_appkit::player_hud::loaded::load;
use plx_machine::machine::{InputOwner, Tick};
use plx_ui::fixture::FixtureMeasure;

const ENTRY: EntryId = EntryId(9);

fn cx() -> Cx<'static, TestHost> {
    Cx {
        views: (),
        tick: Tick::default(),
        measure: &FixtureMeasure,
        focus: Default::default(),
        press: Default::default(),
        owner: InputOwner::Entry(ENTRY),
    }
}

/// `builtin:classic` with its bar pushed up 64 px and in 100 px from the left, and a sixth control in
/// the row: every consumer under test reads a number that differs from classic's.
fn moved() -> Arc<Compiled> {
    let edit = |text: String, from: &str, to: &str| {
        assert!(text.matches(from).count() == 1, "the fixture has exactly one `{from}`");
        text.replace(from, to)
    };
    let text = edit(CLASSIC_PACKAGE.to_owned(), r#""id": "bar", "when": "!sections", "chrome": "steady", "gap_before": 26"#, r#""id": "bar", "when": "!sections", "chrome": "steady", "gap_before": 90"#);
    let text = edit(text, r#""x_inset": 0"#, r#""x_inset": 100"#);
    let more = r#"{ "id": "more", "command": "open_more", "widget": "disc", "size": 64, "rest": "film", "focus": "accent" }"#;
    let text = edit(text, more, &format!(r#"{more}, {}"#, more.replace(r#""id": "more""#, r#""id": "extra""#)));
    load(text.as_bytes(), "test", plx_platform::i18n::english(), &FixtureMeasure).expect("the edited package loads").compiled
}

/// The state the default page (`Discs`, no chapters, not paused) is in, as the compile reports it.
fn state_geometry(compiled: &Compiled) -> plx_appkit::player_hud::geometry::HudGeometry {
    compiled.lookup(0, Slot::Discs, false, false, false).expect("the compact state").geometry(SkipKind::Intro)
}

fn page_with(compiled: Option<Arc<Compiled>>) -> PlayerScreen {
    let mut page = PlayerScreen::new(ENTRY);
    page.compiled = compiled;
    page
}

fn row_spec(page: &PlayerScreen) -> GroupSpec {
    let mut groups = Vec::new();
    Focusable::<TestHost>::groups(page, &cx(), &mut groups);
    groups.into_iter().find(|g| g.id == GROUP_ROW).expect("the row group")
}

/// The test is only worth anything if the package really differs from classic where it is read.
#[test]
fn the_edited_package_differs_from_classic_where_the_consumers_read() {
    let want = state_geometry(&moved());
    let classic = player_hud::classic::frame_geometry();
    assert_ne!(want.grab, classic.grab, "the bar moved");
    assert_ne!(want.playbar.x, classic.playbar.x, "the bar moved in");
    assert_ne!(want.band, classic.band, "the band moved with the bar");
    assert_eq!(want.band(), moved().band());
}

/// The scrubber's stop and the discs' stops are where the compile put them.
#[test]
fn the_pointer_stops_and_the_focus_rects_are_the_compiled_packages() {
    let _g = plx_base::testlock::serial();
    let compiled = moved();
    let want = state_geometry(&compiled);
    let page = page_with(Some(compiled));
    let scrub = Focusable::<TestHost>::place(&page, &player_hud::ELEM_SCRUB, &cx(), At::Drawn).expect("the scrubber places");
    assert_eq!(scrub.rect, want.grab, "the grab band is the package's");
    let tab = Focusable::<TestHost>::place(&page, &player_hud::ELEM_TAB_BASE, &cx(), At::Drawn);
    assert_eq!(tab.map(|p| p.rect), want.tabs[0], "the Info pill is the package's");
    assert_eq!(row_spec(&page).extent, want.row, "the row group's extent is the package's");
}

/// The row group holds a stop for every control of the package's row, not classic's five.
#[test]
fn the_row_group_is_as_long_as_the_packages_row() {
    let _g = plx_base::testlock::serial();
    let compiled = moved();
    let declared = compiled.lookup(0, Slot::Discs, false, false, false).expect("the compact state").row_ids.len();
    assert_eq!(declared, 6, "the sixth control is in the row");
    assert_eq!(row_spec(&page_with(Some(compiled))).len, 6);
    assert_eq!(row_spec(&page_with(None)).len, 5, "classic's row, with no package");
}

/// The caption band and the pointer's mapping onto the bar take the package's numbers.
#[test]
fn the_caption_band_and_the_scrub_mapping_are_the_compiled_packages() {
    let _g = plx_base::testlock::serial();
    let compiled = moved();
    let want = state_geometry(&compiled);
    let page = page_with(Some(compiled));
    assert_eq!(page.caption_band(false), want.band, "the band the captions lift to");
    assert_ne!(page.caption_band(false), page_with(None).caption_band(false));
    // a quarter of the way along the PACKAGE's bar is a quarter of the way through the item (the inset
    // is symmetric, so the middle would not tell the two bars apart)
    let quarter = want.playbar.x + want.playbar.w * 0.25;
    let dur = 1_000_000_000i64;
    let at = page.pointed_ns(quarter, false, dur);
    assert!((at - dur / 4).abs() < dur / 100, "a pointer a quarter along the package's bar is a quarter in: {at}");
    let classic_at = page_with(None).pointed_ns(quarter, false, dur);
    assert!((classic_at - dur / 4).abs() > dur / 100, "classic's bar is somewhere else: {classic_at}");
}

/// No package, or one with no state for the frame, answers classic's report: the draw takes that exit too.
#[test]
fn a_page_with_no_package_places_at_classic() {
    let _g = plx_base::testlock::serial();
    let page = page_with(None);
    let classic = player_hud::classic::frame_geometry();
    let scrub = Focusable::<TestHost>::place(&page, &player_hud::ELEM_SCRUB, &cx(), At::Drawn).expect("the scrubber places");
    assert_eq!(scrub.rect, classic.grab);
    assert_eq!(page.caption_band(false), classic.band);
}
