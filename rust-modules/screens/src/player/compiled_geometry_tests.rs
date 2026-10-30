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

/// `builtin:classic`, the package the app hands the page today.
fn classic() -> Arc<Compiled> {
    load(CLASSIC_PACKAGE.as_bytes(), "test", plx_platform::i18n::english(), &FixtureMeasure).expect("classic loads").compiled
}

/// The three openers (the Subtitles, Audio and `…` discs) and the failure read-out's quality entry, which
/// has no disc, each get an anchor that puts the panel where the constants put it: the segment's outer edge
/// is `CTRL_RIGHT` and the band is the disc row's, so `anchor_panel` settles the panel at `CTRL_RIGHT - pw`.
#[test]
fn classic_menus_hang_from_the_row_ending_at_the_segment_edge() {
    let _g = plx_base::testlock::serial();
    let page = page_with(Some(classic()));
    for kind in [
        overlay::OverlayKind::Tracks { tab: 0 },
        overlay::OverlayKind::Tracks { tab: 1 },
        overlay::OverlayKind::More { page: None },
        overlay::OverlayKind::More { page: Some(plx_appkit::more_menu::MorePage::Quality) },
    ] {
        let anchor = page.menu_anchor(kind, false).expect("a menu has an anchor under a package");
        assert_eq!(anchor.x + anchor.w, player_hud::classic::CTRL_RIGHT, "{kind:?}: the segment's outer edge");
        assert_eq!((anchor.y, anchor.h), (player_hud::classic::CTRL_Y, player_hud::classic::CTRL_H), "{kind:?}: the row's band");
    }
}

/// A panel that is not a menu has no anchor to be handed, and neither has a page with no package: the
/// mount then keeps the menu's own default (classic's row).
#[test]
fn only_menus_under_a_package_have_an_anchor() {
    let _g = plx_base::testlock::serial();
    let page = page_with(Some(classic()));
    for kind in [overlay::OverlayKind::Info, overlay::OverlayKind::Chapters, overlay::OverlayKind::Timing] {
        assert_eq!(page.menu_anchor(kind, false), None, "{kind:?}");
    }
    assert_eq!(page_with(None).menu_anchor(overlay::OverlayKind::Tracks { tab: 0 }, false), None);
}

/// The anchor is the COMPILE's: the opener the engine holds when its command opens the menu, else the first
/// control of the state that does. Here the package has a second `open_more` after the first, so the two
/// answers differ, and each is `CompiledState::menu_anchor` of its control.
#[test]
fn the_menu_anchor_is_the_compiled_openers_and_follows_the_held_control() {
    let _g = plx_base::testlock::serial();
    let compiled = moved();
    let state = compiled.lookup(0, Slot::Discs, false, false, false).expect("the compact state");
    let index_of = |id: &str| state.controls.iter().position(|c| c.id == id).expect(id);
    let (more, extra) = (index_of("more"), index_of("extra"));
    let (more_anchor, extra_anchor) = (state.menu_anchor(more).unwrap(), state.menu_anchor(extra).unwrap());
    assert_ne!(more_anchor, extra_anchor, "the two openers are at different rest rects");

    let mut page = page_with(Some(Arc::clone(&compiled)));
    let kind = overlay::OverlayKind::More { page: None };
    assert_eq!(page.menu_anchor(kind, false), Some(more_anchor), "nothing held: the first opener of the command");
    page.hud.nav.focus = GROUP_ROW;
    page.hud.nav.btn = i32::try_from(state.row_ids.iter().position(|id| id.as_deref() == Some("extra")).expect("in the row")).unwrap();
    assert_eq!(page.menu_anchor(kind, false), Some(extra_anchor), "the held control opens it");
    // the held control opens a DIFFERENT command: the first opener of the asked one is used
    let tracks = overlay::OverlayKind::Tracks { tab: 0 };
    let first_tracks = state.controls.iter().position(|c| c.command == "open_tracks").expect("an opener");
    assert_eq!(page.menu_anchor(tracks, false), state.menu_anchor(first_tracks));
}
