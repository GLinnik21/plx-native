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
//! The other pins of the page build it on `builtin:classic` (`PlayerScreen::new(ENTRY, classic_for_test())`):
//! a page always holds a compile, and the frame a package has no state for has one answer, the empty report
//! (`a_package_with_no_state_for_the_frame_answers_the_empty_report`).

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

fn page_with(compiled: Arc<Compiled>) -> PlayerScreen {
    PlayerScreen::new(ENTRY, compiled)
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
    let classic = state_geometry(&classic());
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
    let page = page_with(compiled);
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
    assert_eq!(row_spec(&page_with(compiled)).len, 6);
    assert_eq!(row_spec(&page_with(classic())).len, 5, "classic's row");
}

/// The caption band and the pointer's mapping onto the bar take the package's numbers.
#[test]
fn the_caption_band_and_the_scrub_mapping_are_the_compiled_packages() {
    let _g = plx_base::testlock::serial();
    let compiled = moved();
    let want = state_geometry(&compiled);
    let page = page_with(compiled);
    assert_eq!(page.caption_band(), want.band, "the band the captions lift to");
    assert_ne!(page.caption_band(), page_with(classic()).caption_band());
    // a quarter of the way along the PACKAGE's bar is a quarter of the way through the item (the inset
    // is symmetric, so the middle would not tell the two bars apart)
    let quarter = want.playbar.x + want.playbar.w * 0.25;
    let dur = 1_000_000_000i64;
    let at = page.pointed_ns(quarter, false, dur);
    assert!((at - dur / 4).abs() < dur / 100, "a pointer a quarter along the package's bar is a quarter in: {at}");
    let classic_at = page_with(classic()).pointed_ns(quarter, false, dur);
    assert!((classic_at - dur / 4).abs() > dur / 100, "classic's bar is somewhere else: {classic_at}");
}

/// `builtin:classic` places at classic's own rects: the scrubber's grab band and the caption band are the
/// ones the audit grades (`player_hud::audit_geometry`).
#[test]
fn a_page_on_builtin_classic_places_at_classic() {
    let _g = plx_base::testlock::serial();
    let page = page_with(classic());
    let want = player_hud::audit_geometry();
    let scrub = Focusable::<TestHost>::place(&page, &player_hud::ELEM_SCRUB, &cx(), At::Drawn).expect("the scrubber places");
    assert_eq!(scrub.rect, want.grab);
    assert_eq!(page.caption_band(), want.band);
}

/// **A package with no state for the frame answers ONE thing for the draw and for the geometry**: nothing is
/// drawn (and the page says so once, not every frame), the stops have no rect and the row no stop, no menu has
/// an anchor, and the caption band is the package's own. `Compiled::default()` has no state for any frame.
#[test]
fn a_package_with_no_state_for_the_frame_answers_the_empty_report() {
    let _g = plx_base::testlock::serial();
    let none = Arc::new(Compiled::default());
    let mut page = page_with(Arc::clone(&none));
    let empty = player_hud::geometry::HudGeometry::empty(none.band());
    assert_eq!(page.geometry_for(false), empty);
    assert_eq!(page.caption_band(), none.band());
    assert_eq!(page.row_len(false), 0, "a stop with no rect cannot be placed");
    assert_eq!(row_spec(&page).extent, Rect::default(), "…and the row group has no extent");
    assert_eq!(page.menu_anchor(overlay::OverlayKind::Tracks { tab: 0 }, false), None);
    assert!(!page.row_enabled(0));
    assert_eq!(page.row_command(0), None);
    let scrub = Focusable::<TestHost>::place(&page, &player_hud::ELEM_SCRUB, &cx(), At::Drawn);
    assert!(scrub.is_none_or(|p| p.rect == Rect::default()), "the scrubber has no rect to grab");
    // the draw takes the same exit: nothing painted, one log line however many frames
    let ps = plx_media::route::PlaybackSession::IDLE;
    let store = plx_data::stores::metadata::MetadataStore::default();
    assert!(!page.render.no_state_logged);
    for _ in 0..3 {
        let drawn = plx_ui::draw_census::capture(|| page.draw_hud(&ps, 0, &FixtureMeasure, store.view()));
        assert!(drawn.is_empty(), "nothing is drawn for a frame the package has no state for: {drawn:?}");
    }
    assert!(page.render.no_state_logged, "…and it was reported");
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
    let page = page_with(classic());
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

/// A panel that is not a menu has no anchor to be handed: the mount then keeps the menu's own default
/// (classic's row).
#[test]
fn only_menus_under_a_package_have_an_anchor() {
    let _g = plx_base::testlock::serial();
    let page = page_with(classic());
    for kind in [overlay::OverlayKind::Info, overlay::OverlayKind::Chapters, overlay::OverlayKind::Timing] {
        assert_eq!(page.menu_anchor(kind, false), None, "{kind:?}");
    }
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

    let mut page = page_with(Arc::clone(&compiled));
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

// ---------------------------------------------------------------------------------------------------
// PR7 stage 1d: a press performs the command of the control the PACKAGE placed
// ---------------------------------------------------------------------------------------------------

fn reference() -> Arc<Compiled> {
    let text = include_str!("../../../appkit/src/hud_format/fixtures/reference.plxhud.json");
    load(text.as_bytes(), "test", plx_platform::i18n::english(), &FixtureMeasure).expect("the reference loads").compiled
}

/// The row's stable index of the control `id` in the reference's movie form.
fn ordinal(compiled: &Compiled, id: &str) -> i32 {
    let state = compiled.lookup(0, Slot::Discs, false, false, false).expect("the compact state");
    state.row_ids.iter().position(|r| r.as_deref() == Some(id)).unwrap_or_else(|| panic!("no `{id}` in the row")) as i32
}

/// **A control past classic's five performs its own command.** The page resolved a press through classic's
/// table (`slot.command(i)`), so under the reference the Play disc asked for whatever classic's fourth
/// control is. The page now asks the package's control at that index, with its parameters.
#[test]
fn a_press_asks_for_the_command_of_the_control_the_package_placed() {
    use plx_appkit::player_hud::command::{HudCommand, MorePage, SeekBy, TrackTab};
    use std::num::NonZeroI32;
    let _g = plx_base::testlock::serial();
    let compiled = reference();
    let secs = |n| HudCommand::Seek(SeekBy::Secs(NonZeroI32::new(n).unwrap()));
    let mut page = page_with(Arc::clone(&compiled));
    page.model.duration_ns = 3_600_000_000_000;
    page.hud.visible_at_press = true;
    page.hud.nav.focus = GROUP_ROW;
    let mut rig = super::travel_rig::Rig::on(&page, ENTRY);
    for (id, want) in [
        ("play", HudCommand::TogglePlay),
        ("back", secs(-10)),
        ("fwd", secs(30)),
        ("subs", HudCommand::OpenTracks(TrackTab::Subtitles)),
        ("more", HudCommand::OpenMore(MorePage::Root)),
    ] {
        let i = ordinal(&compiled, id);
        rig.seat_on(player_hud::control_elem(i));
        let (_, reqs) = rig.deliver(&mut page, ScreenEvent::PressCommit(plx_machine::machine::PressId(1)));
        assert_eq!(reqs, vec![PlayerReq::Command(want)], "`{id}` at index {i}");
        assert_eq!(page.hud.nav.btn, i, "the cursor is seated on the pressed control");
    }
}

/// **A control is enabled by its own command's predicate** (`HudCommand::enabled`): a seek needs a known
/// duration, an absent Previous/Next has no stop, and the rest are always there.
#[test]
fn a_package_controls_stop_is_its_commands_enabled_predicate() {
    let _g = plx_base::testlock::serial();
    let compiled = reference();
    let mut page = page_with(Arc::clone(&compiled));
    let (back, play, prev) = (ordinal(&compiled, "back"), ordinal(&compiled, "play"), 2);
    assert!(page.row_enabled(play));
    assert!(!page.row_enabled(back), "no duration, no jump");
    page.model.duration_ns = 1;
    assert!(page.row_enabled(back));
    assert!(!page.row_enabled(prev), "a movie has no Previous: its index is held, its stop is not");
    assert!(!page.row_enabled(40), "past the row");
}

/// The cursor parks, when the row appears, on Next, else Previous, else Play, else the first enabled control
/// (classic's rule over commands: its Next, else Previous, else Subtitles).
#[test]
fn the_row_parks_on_the_primary_control_of_the_package() {
    let _g = plx_base::testlock::serial();
    let compiled = reference();
    let mut page = page_with(Arc::clone(&compiled));
    assert_eq!(page.row_primary(), ordinal(&compiled, "play"), "a movie: no pair, so Play");
    page.slot = ControlSlot::Discs(plx_appkit::player_hud::Controls::for_item(true, true, true, false));
    let episode = compiled.lookup(0, Slot::Discs, false, true, false).expect("the episode form");
    let next = episode.row_ids.iter().position(|r| r.as_deref() == Some("next")).unwrap() as i32;
    assert_eq!(page.row_primary(), next, "an episode with a Next: Next");
    assert_eq!(page_with(classic()).row_primary(), page_with(classic()).slot.primary_btn(), "classic's rule over its own table");
}
