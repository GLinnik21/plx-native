//! **The transitions of the renderer** (`docs/hud-plugins.md`, D6): the bands of the transport that a section's
//! page moves. The rising-tabs package lays its tabs ABOVE the section, so they travel when a section opens,
//! closes or is switched for the other; its title block and bar exist only with no section open, so they fade on
//! the page's own spring ([`HudMotion::page_appear`]); classic is the same rule where nothing travels, and the
//! reference is an inline package, which keeps its lift and fades what only the compact state holds.
//!
//! Every test draws a chosen point of the motion (`set_section_appear_for_test`, `set_page_for_test`) or steps the
//! real springs, and reads what the painter was handed: the labels' places, the fills' effective alpha, the
//! scrim's rect.

use super::*;
use crate::hud_format::compile::{OPEN_CHAPTERS, OPEN_INFO};
use crate::hud_format::Slot;
use plx_data::metadata::Chapter;
use plx_machine::present::Present;
use plx_ui::consts::{SCR_H, SCR_W};
use std::sync::Arc;

const NONE: ControlSlot = ControlSlot::Discs(Controls::NONE);
const RISING: &str = include_str!("../../../hud_format/fixtures/rising-tabs.plxhud.json");
const REFERENCE: &str = include_str!("../../../hud_format/fixtures/reference.plxhud.json");

fn load(text: &str) -> Arc<Compiled> {
    crate::player_hud::loaded::load(text.as_bytes(), "0.0.0-test", plx_platform::i18n::english(), &FM)
        .expect("the package loads")
        .compiled
}

fn rising() -> Arc<Compiled> {
    load(RISING)
}

fn classic() -> Arc<Compiled> {
    crate::player_hud::loaded::load_classic("0.0.0-test", plx_platform::i18n::english(), &FM).compiled
}

/// `text` with `from` written as `to`, which must occur once.
fn edit(text: &str, from: &str, to: &str) -> String {
    assert_eq!(text.matches(from).count(), 1, "`{from}` occurs once in the package");
    text.replacen(from, to, 1)
}

/// A playing item with chapters, so the packages' Chapters states are the ones a frame selects.
fn store() -> plx_data::stores::metadata::MetadataStore {
    let mut store = plx_data::stores::metadata::MetadataStore::default();
    let chapters = (0..3)
        .map(|i| Chapter { index: i + 1, start_ms: i * 125_000, title: format!("Chapter {}", i + 1), thumb: format!("/library/metadata/{i}/thumb") })
        .collect();
    plx_data::metadata::set_playing_chapters_for_test(store.state_mut(), chapters);
    store
}

/// One frame of `compiled` through [`draw`], as the screen hands it: `open` the logical sections, `slot` the
/// occupant it draws, `transport` whether the middle is up.
fn draw_in(compiled: &Compiled, row: &mut HudMotion, store: &plx_data::stores::metadata::MetadataStore, open: u8, slot: ControlSlot, transport: bool) -> Census {
    let ps = plx_media::route::PlaybackSession::IDLE;
    let up = Countdown::default();
    let model = HudModel { duration_ns: 600_000_000_000, playpos_ns: 90_000_000_000, ..HudModel::default() };
    let frame = Frame {
        entry: EntryId(0), ps: &ps, model: &model, up: &up, slot, busy: Busy::None, focus: GROUP_SCRUB, btn: 0, tab: 0,
        open, now: 1_000, transport, measure: &FM, meta: store.view(),
    };
    census(|| assert!(with_df(|df| draw(compiled, &frame, df, row, Painter::root())), "the package has a state for the frame"))
}

/// The same frame through [`draw_state`] over `state`: a state walked on its own, every section at rest.
fn walk_in(compiled: &Compiled, state: &CompiledState, row: &mut HudMotion, store: &plx_data::stores::metadata::MetadataStore, open: u8, transport: bool) -> Census {
    let ps = plx_media::route::PlaybackSession::IDLE;
    let up = Countdown::default();
    let model = HudModel { duration_ns: 600_000_000_000, playpos_ns: 90_000_000_000, ..HudModel::default() };
    let frame = Frame {
        entry: EntryId(0), ps: &ps, model: &model, up: &up, slot: NONE, busy: Busy::None, focus: GROUP_SCRUB, btn: 0, tab: 0,
        open, now: 1_000, transport, measure: &FM, meta: store.view(),
    };
    census(|| with_df(|df| draw_state(compiled, state, &frame, df, row, Painter::root())))
}

/// The state of the `open` bits over the discs, with chapters and a movie.
fn state(compiled: &Compiled, open: u8) -> &CompiledState {
    compiled.lookup(open, Slot::Discs, true, false, false).unwrap_or_else(|| panic!("a state for open={open}"))
}

fn band(state: &CompiledState, id: &str) -> Rect {
    state.bands.iter().find(|(b, _)| b == id).map(|(_, r)| *r).unwrap_or_else(|| panic!("no band `{id}` in {:?}", state.bands))
}

fn label_y(c: &Census, label: &str) -> f32 {
    c.1.iter().find(|r| r.text == label).unwrap_or_else(|| panic!("no `{label}` run in {:?}", c.1)).at.y
}

fn clock(c: &Census) -> Option<&Run> {
    c.1.iter().find(|r| r.text == "1:30")
}

/// The effective alpha of the title block's two runs (the kicker and the title: empty on the host, which has no
/// playing item, but each is still drawn and carries its effective ink), found by their left edge `left` and the
/// title's size `big`. The section states' own texts stand elsewhere, and the bands a test adds use other sizes.
fn title_alphas(c: &Census, left: f32, big: i32) -> Vec<f32> {
    c.1.iter()
        .filter(|r| r.text.is_empty() && near(r.at.x, left) && (r.size == plx_ui::theme::size::CAPTION || r.size == big))
        .map(|r| r.ink[3])
        .collect()
}

fn all_near(alphas: &[f32], want: f32) -> bool {
    !alphas.is_empty() && alphas.iter().all(|a| (a - want).abs() < 0.001)
}

/// The effective alpha of the bar: its clock and its fill.
fn bar_alphas(c: &Census) -> Vec<f32> {
    let mut out: Vec<f32> = c.1.iter().filter(|r| r.text == "1:30" || r.text == "-8:30").map(|r| r.ink[3]).collect();
    out.extend(c.3.iter().filter(|(_, r, _)| near(r.h, 6.0) || near(r.h, 8.0)).map(|(_, _, a)| *a));
    out
}

fn has_discs(c: &Census) -> bool {
    c.2.iter().any(|(icon, _)| *icon == Icon::More)
}

/// The scrim's rect: the primitive that spans the screen and ends at its bottom.
fn scrim_of(c: &Census) -> Rect {
    c.0.iter().map(|(_, r)| *r).find(|r| r.x == 0.0 && r.w == SCR_W && (r.y + r.h - SCR_H).abs() < 0.01).unwrap_or_else(|| panic!("no scrim in {:?}", c.0))
}

fn near(a: f32, b: f32) -> bool {
    (a - b).abs() < 0.01
}

/// A fresh motion with `section` at `appear` (opening) and the page at `page`, as the tests that draw a chosen point
/// of the motion build theirs.
fn motion(section: Section, appear: f32, page: f32) -> HudMotion {
    let mut row = HudMotion::new();
    row.set_section_appear_for_test(section, appear);
    row.set_page_for_test(page);
    row
}

/// **At the end points the draw is the settled state's, op for op.** A package whose sections are modal draws its
/// compact state with nothing open, and the section's state alone with the section up: through [`draw`] it is the
/// census [`draw_state`] gives the state walked on its own.
#[test]
fn at_the_end_points_the_census_is_the_settled_states() {
    let _serial = plx_base::testlock::serial();
    let _en = language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    let store = store();
    for (name, compiled) in [("rising-tabs", rising()), ("classic", classic())] {
        let compact = state(&compiled, 0);
        let drawn = draw_in(&compiled, &mut HudMotion::new(), &store, 0, NONE, true);
        let walked = walk_in(&compiled, compact, &mut HudMotion::new(), &store, 0, true);
        assert_eq!(census_hash(&drawn), census_hash(&walked), "{name}: page 0, nothing open");
        for (open, section) in [(OPEN_INFO, Section::Info), (OPEN_CHAPTERS, Section::Chapters)] {
            let drawn = draw_in(&compiled, &mut motion(section, 1.0, 1.0), &store, open, NONE, false);
            let walked = walk_in(&compiled, state(&compiled, open), &mut motion(section, 1.0, 1.0), &store, open, false);
            assert_eq!(census_hash(&drawn), census_hash(&walked), "{name}: page 1, section {open} up");
        }
    }
}

/// **Rising tabs at the half of a section's opening.** The tabs stand halfway between their place in the compact
/// state and their place in the Info state; the title block (the top band) and the bar are seen at half; the discs
/// are still drawn; the scrim's top edge is half way between its two tops and its bottom is the screen's.
#[test]
fn halfway_the_tabs_stand_half_way_and_the_title_block_is_half_seen() {
    let _serial = plx_base::testlock::serial();
    let _en = language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    let (compiled, store) = (rising(), store());
    let (compact, info) = (state(&compiled, 0), state(&compiled, OPEN_INFO));
    let (c, m) = (band(compact, "tabs"), band(info, "tabs"));
    assert!(m.y < c.y - 100.0, "the tabs rise when a section opens: {c:?} to {m:?}");

    let at_compact = draw_in(&compiled, &mut HudMotion::new(), &store, 0, NONE, true);
    let at_info = draw_in(&compiled, &mut motion(Section::Info, 1.0, 1.0), &store, OPEN_INFO, NONE, false);
    let off = label_y(&at_compact, "Info") - c.y;
    assert!(near(label_y(&at_info, "Info") - m.y, off), "a tab's label stands the same in its band wherever the band is");

    let mid = draw_in(&compiled, &mut motion(Section::Info, 0.5, 0.5), &store, OPEN_INFO, NONE, false);
    assert!(near(label_y(&mid, "Info"), c.y + 0.5 * (m.y - c.y) + off), "the tab stands half way: {} between {} and {}", label_y(&mid, "Info"), c.y, m.y);
    assert_eq!(mid.1.iter().filter(|r| r.text == "Info").count(), 1, "and once: the tab is not drawn by two walks");
    let ink = |c: &Census| c.1.iter().find(|r| r.text == "Info").unwrap().ink[3];
    assert!(near(ink(&mid), ink(&at_info)), "the tab does not fade");

    assert!(all_near(&title_alphas(&at_compact, 136.0, 40), 1.0), "the title block is whole at rest");
    assert!(all_near(&title_alphas(&mid, 136.0, 40), 0.5), "…and half seen with the page half up: {:?}", title_alphas(&mid, 136.0, 40));
    assert!(has_discs(&mid), "its discs are drawn too, on the screen's own slot");
    let (rest, faded) = (clock(&at_compact).expect("the bar's clock"), clock(&mid).expect("the bar's clock at half"));
    assert!(near(faded.at.y, rest.at.y), "the bar fades where it stands: {} against {}", faded.at.y, rest.at.y);
    assert!((faded.ink[3] - 0.5 * rest.ink[3]).abs() < 0.001, "…at half its ink: {} of {}", faded.ink[3], rest.ink[3]);
    assert!(all_near(&bar_alphas(&mid), 0.5), "…and its fill: {:?}", bar_alphas(&mid));

    let (sc, sm) = (compact.scrim.expect("a scrim"), info.scrim.expect("a scrim"));
    assert!(sm.y < sc.y - 1.0 && near(sc.y + sc.h, SCR_H) && near(sm.y + sm.h, SCR_H), "the block scrim rises with the section: {sc:?} to {sm:?}");
    let s = scrim_of(&mid);
    assert!(near(s.y, sc.y + 0.5 * (sm.y - sc.y)), "its top edge is half way: {s:?} between {sc:?} and {sm:?}");
    assert!(near(s.y + s.h, SCR_H), "…and its bottom is the screen's, not translated: {s:?}");
}

/// **The first frame of a section is the compact HUD whole**: the section held at appear 0 (and the page with it)
/// draws the title block and bar at full ink and the tabs where the compact state stands; at page 1 with the section
/// settled nothing of the compact state is drawn.
#[test]
fn the_held_frame_is_the_compact_hud_whole_and_the_up_frame_has_none_of_it() {
    let _serial = plx_base::testlock::serial();
    let _en = language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    let (compiled, store) = (rising(), store());
    let compact = state(&compiled, 0);
    let at_compact = draw_in(&compiled, &mut HudMotion::new(), &store, 0, NONE, true);

    let held = draw_in(&compiled, &mut motion(Section::Info, 0.0, 0.0), &store, OPEN_INFO, NONE, false);
    assert!(near(label_y(&held, "Info"), label_y(&at_compact, "Info")), "the tabs stand in the compact state's place");
    assert!(all_near(&title_alphas(&held, 136.0, 40), 1.0), "the title block is whole on the held frame");
    assert!(all_near(&bar_alphas(&held), 1.0), "…and the bar");
    assert!(near(scrim_of(&held).y, compact.scrim.unwrap().y), "…and the scrim is the compact one");

    let up = draw_in(&compiled, &mut motion(Section::Info, 1.0, 1.0), &store, OPEN_INFO, NONE, false);
    assert!(clock(&up).is_none() && title_alphas(&up, 136.0, 40).is_empty() && !has_discs(&up), "nothing of the compact bands remains once the page is up");
}

/// **A press that turns the motion round leaves no jump**, with the real springs: the page and the tabs are one
/// number whichever way it runs, so at every frame of an opening, a close and a turn in the middle of either, the
/// tabs stand `page` of the way between their two places and the title block is seen at `1 - page`.
#[test]
fn a_press_turning_the_motion_round_has_no_jump() {
    let _serial = plx_base::testlock::serial();
    let _en = language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    let (compiled, store) = (rising(), store());
    let (compact, info) = (state(&compiled, 0), state(&compiled, OPEN_INFO));
    let (c, m) = (band(compact, "tabs").y, band(info, "tabs").y);
    let off = label_y(&draw_in(&compiled, &mut HudMotion::new(), &store, 0, NONE, true), "Info") - c;

    let mut row = HudMotion::new();
    let mut present = Present::new();
    let ms = std::cell::Cell::new(1_000u32);
    let last = std::cell::Cell::new(None::<f32>);
    // one frame as the screen hands it: `open` the logical sections, the middle up when none is
    let look = |row: &mut HudMotion, open: u8, what: &str| {
        let drew = draw_in(&compiled, row, &store, open, NONE, open == 0);
        let (y, page) = (label_y(&drew, "Info") - off, row.page_appear());
        assert!((y - (c + page * (m - c))).abs() < 0.05, "{what}: the tabs stand at {y}, page {page} puts them at {}", c + page * (m - c));
        let seen = title_alphas(&drew, 136.0, 40);
        if page < 0.999 {
            assert!(!seen.is_empty() && seen.iter().all(|a| (a - (1.0 - page)).abs() < 0.003), "{what}: the title block is seen at {} not {seen:?}", 1.0 - page);
        } else {
            assert!(seen.is_empty(), "{what}: with the page up the title block is gone: {seen:?}");
        }
        if let Some(before) = last.get() {
            assert!((y - before).abs() < 0.4 * (c - m), "{what}: the tabs jumped from {before} to {y}");
        }
        last.set(Some(y));
        y
    };
    let mut step = |row: &mut HudMotion| {
        row.step_appear(Tick { ms: ms.get(), dt_us: 16_667 }, &mut plx_machine::machine::PresentHandle::of(&mut present));
        ms.set(ms.get() + 16);
    };

    row.follow(OPEN_INFO);
    look(&mut row, OPEN_INFO, "the opening frame");
    // the spring is held at its first frame until the text it draws is ready: the frames of the hold are the compact
    // HUD whole, and the first step after it starts the motion
    let mut held = 0;
    while row.page_appear() == 0.0 {
        step(&mut row);
        look(&mut row, OPEN_INFO, &format!("held, frame {held}"));
        held += 1;
        assert!(held < 100, "the page leaves its first frame");
    }
    for i in 0..5 {
        step(&mut row);
        look(&mut row, OPEN_INFO, &format!("opening, frame {i}"));
    }
    let at_press = last.get().unwrap();
    assert!(at_press < c && at_press > m, "the press lands mid-way: {at_press} between {c} and {m} (page {})", row.page_appear());

    // the key that turns it round: the page puts the section away; nothing moves until the dismissal frame has passed
    row.hold_focus(Section::Info, GROUP_TABS, 0);
    row.follow(0);
    row.dismissed_on(Section::Info, ms.get());
    assert!(near(look(&mut row, 0, "the press"), at_press), "the press itself moves nothing");
    step(&mut row);
    assert!(near(look(&mut row, 0, "the dismissal frame"), at_press), "the dismissal frame draws what the press left");
    // the spring keeps the velocity it had when the key turned it round, so it may run on a little before it comes
    // back: the tabs follow it, with no jump (the check of `look`)
    let mut y = at_press;
    for i in 0..8 {
        step(&mut row);
        y = look(&mut row, 0, &format!("closing, frame {i}"));
    }
    assert!(y > at_press, "the tabs are on their way back down: {y} after {at_press}");
    // …and turned round again mid-way, back up
    row.follow(OPEN_INFO);
    let turned = look(&mut row, OPEN_INFO, "the second press");
    assert!(near(turned, y), "the second press moves nothing either");
    for i in 0..60 {
        step(&mut row);
        look(&mut row, OPEN_INFO, &format!("opening again, frame {i}"));
    }
    assert!(near(last.get().unwrap(), m), "the tabs end where the Info state puts them: {} and {m}", last.get().unwrap());
}

/// **A switch moves the tabs between the two sections' places and keeps the title block gone**: Chapters
/// fading and Info rising, the page at 1, the tabs stand at the mean of their two places weighted by the sections'
/// appear (so the two never sum to anything the tabs must follow), once, and nothing of the compact bands is seen.
#[test]
fn a_switch_moves_the_tabs_between_the_two_sections_places_with_the_title_block_gone() {
    let _serial = plx_base::testlock::serial();
    let _en = language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    let (compiled, store) = (rising(), store());
    let (chapters, info) = (state(&compiled, OPEN_CHAPTERS), state(&compiled, OPEN_INFO));
    let (yc, yi) = (band(chapters, "tabs").y, band(info, "tabs").y);
    assert!((yc - yi).abs() > 5.0, "the two sections put the tabs in two places: {yc} and {yi}");
    let compact = state(&compiled, 0);
    let off = label_y(&draw_in(&compiled, &mut HudMotion::new(), &store, 0, NONE, true), "Info") - band(compact, "tabs").y;

    for (a_chapters, a_info) in [(0.9, 0.1), (0.5, 0.5), (0.3, 0.7), (0.02, 0.98)] {
        let mut row = HudMotion::new();
        row.set_section_appear_for_test(Section::Chapters, a_chapters);
        row.set_section_appear_for_test(Section::Info, a_info);
        row.follow(OPEN_INFO);
        row.set_page_for_test(1.0);
        let drew = draw_in(&compiled, &mut row, &store, OPEN_INFO, NONE, false);
        let mean = (a_chapters * yc + a_info * yi) / (a_chapters + a_info);
        assert!(near(label_y(&drew, "Info") - off, mean), "chapters {a_chapters}, info {a_info}: the tabs stand at {} not the weighted mean {mean}", label_y(&drew, "Info") - off);
        assert_eq!(drew.1.iter().filter(|r| r.text == "Info").count(), 1, "the tab is drawn once, not once per section");
        assert!(clock(&drew).is_none(), "the bar is gone");
        assert!(!has_discs(&drew), "…and the title block's discs: {:?}", drew.2);
        assert!(title_alphas(&drew, 136.0, 40).is_empty(), "nothing of the title block is drawn at all");
    }
}

/// Classic's tabs do not move (it is bottom-anchored, so they stand at one y in every state), its scrim is fixed,
/// and the title block, bar and discs are seen at `1 - page`.
#[test]
fn classic_in_motion_fades_its_title_bar_and_row_and_moves_nothing_else() {
    let _serial = plx_base::testlock::serial();
    let _en = language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    let (compiled, store) = (classic(), store());
    let (compact, info) = (state(&compiled, 0), state(&compiled, OPEN_INFO));
    assert_eq!(band(compact, "tabs"), band(info, "tabs"), "classic's tabs stand at one place");
    assert_eq!(compact.scrim, info.scrim, "…and its scrim is the same rect");
    let rest = draw_in(&compiled, &mut HudMotion::new(), &store, 0, NONE, true);
    assert!(all_near(&title_alphas(&rest, 96.0, 54), 1.0) && all_near(&bar_alphas(&rest), 1.0), "classic's title block and bar are whole at rest");
    let ink = |c: &Census| c.1.iter().find(|r| r.text == "Info").unwrap().ink[3];
    for page in [0.25, 0.5, 0.75] {
        let mid = draw_in(&compiled, &mut motion(Section::Info, 0.5, page), &store, OPEN_INFO, NONE, false);
        assert!(near(label_y(&mid, "Info"), label_y(&rest, "Info")), "page {page}: the tab did not move");
        assert!(near(ink(&mid), ink(&rest)), "page {page}: nor fade");
        assert_eq!(scrim_of(&mid), compact.scrim.unwrap(), "page {page}: a fixed scrim stands still");
        assert!(all_near(&title_alphas(&mid, 96.0, 54), 1.0 - page), "page {page}: the title block is seen at {}: {:?}", 1.0 - page, title_alphas(&mid, 96.0, 54));
        assert!(all_near(&bar_alphas(&mid), 1.0 - page), "page {page}: the bar is seen at {}: {:?}", 1.0 - page, bar_alphas(&mid));
        assert!(near(clock(&mid).unwrap().at.y, clock(&rest).unwrap().at.y), "page {page}: where it stands");
        assert!(has_discs(&mid), "page {page}: the discs are drawn");
    }
}

/// **An inline package keeps its lift and fades what only the compact state holds.** The reference with its bar
/// written `!sections`: the bands above an appearing section ride the one delta they always rode, the bar is seen at
/// `1 - page` where the compact state stands and is gone with the page up; the block scrim's top edge follows the
/// section's appear (the lift's clock, not the page's).
#[test]
fn an_inline_package_fades_its_compact_only_band_and_keeps_the_lift() {
    let _serial = plx_base::testlock::serial();
    let _en = language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    let compiled = load(&edit(REFERENCE, r#"{ "id": "bar", "gap_before": "SM","#, r#"{ "id": "bar", "when": "!sections", "gap_before": "SM","#));
    let store = store();
    let (compact, info) = (state(&compiled, 0), state(&compiled, OPEN_INFO));
    assert!(info.bands.iter().all(|(id, _)| id != "bar") && compact.bands.iter().any(|(id, _)| id == "bar"), "the bar belongs to the compact state alone");
    let rest = draw_in(&compiled, &mut HudMotion::new(), &store, 0, NONE, true);
    let rest_clock = clock(&rest).expect("the bar's clock at rest");

    for appear in [0.25, 0.5, 0.75] {
        let mid = draw_in(&compiled, &mut motion(Section::Info, appear, appear), &store, OPEN_INFO, NONE, true);
        let bar = clock(&mid).unwrap_or_else(|| panic!("appear {appear}: the fading bar is drawn"));
        assert!((bar.ink[3] - (1.0 - appear) * rest_clock.ink[3]).abs() < 0.001, "appear {appear}: the bar is seen at {}: {}", 1.0 - appear, bar.ink[3]);
        assert!(near(bar.at.y, rest_clock.at.y), "appear {appear}: where the compact state stands, not lifted: {} and {}", bar.at.y, rest_clock.at.y);
        // the title and the row are the lift's: a band in both ends, the one delta lower than the Info state's place
        let title_y = mid.1.iter().find(|r| r.text.is_empty() && r.size == 24 && near(r.at.x, 136.0)).unwrap().at.y;
        let (rest_y, info_y) = (band(compact, "title").y + 0.0, band(info, "title").y);
        assert!(near(title_y, info_y + (1.0 - appear) * (rest_y - info_y)), "appear {appear}: the title rides the lift: {title_y}");
        // the scrim follows the lift's clock, on the page that is behind it
        let (sc, si) = (compact.scrim.unwrap(), info.scrim.unwrap());
        let s = scrim_of(&mid);
        assert!(near(s.y, si.y + (1.0 - appear) * (sc.y - si.y)), "appear {appear}: the scrim's top is {} not the lift's {}", s.y, si.y + (1.0 - appear) * (sc.y - si.y));
        assert!(near(s.y + s.h, SCR_H), "appear {appear}: its bottom stays");
    }
    let up = draw_in(&compiled, &mut motion(Section::Info, 1.0, 1.0), &store, OPEN_INFO, NONE, true);
    assert!(clock(&up).is_none(), "with the page up the bar is gone");
    // the page is already up when a second inline section opens: the scrim still rides the section's own spring
    let mut row = motion(Section::Info, 1.0, 1.0);
    row.set_section_appear_for_test(Section::Chapters, 0.5);
    row.set_page_for_test(1.0);
    let both = draw_in(&compiled, &mut row, &store, OPEN_INFO | OPEN_CHAPTERS, NONE, true);
    let (s_both, s_info) = (state(&compiled, OPEN_INFO | OPEN_CHAPTERS).scrim.unwrap(), info.scrim.unwrap());
    let top = scrim_of(&both).y;
    assert!(top > s_both.y + 0.01 && top < s_info.y + 0.01 || top < s_both.y - 0.01 && top > s_info.y - 0.01, "a second section opening moves the scrim part of the way: {top} between {} and {}", s_info.y, s_both.y);
}

/// A `fixed` scrim is the same rect in every state and stands still; a block scrim follows the tabs' clock. Through
/// a switch the top edge is the weighted mean of the two sections' tops.
#[test]
fn the_scrim_follows_its_top_edge_only_and_a_fixed_one_stands_still() {
    let _serial = plx_base::testlock::serial();
    let _en = language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    let (compiled, store) = (rising(), store());
    let (chapters, info) = (state(&compiled, OPEN_CHAPTERS), state(&compiled, OPEN_INFO));
    let (yc, yi) = (chapters.scrim.unwrap().y, info.scrim.unwrap().y);
    assert!((yc - yi).abs() > 5.0, "the two sections put the scrim's top in two places");
    let mut row = HudMotion::new();
    row.set_section_appear_for_test(Section::Chapters, 0.25);
    row.set_section_appear_for_test(Section::Info, 0.75);
    row.follow(OPEN_INFO);
    row.set_page_for_test(1.0);
    let s = scrim_of(&draw_in(&compiled, &mut row, &store, OPEN_INFO, NONE, false));
    assert!(near(s.y, (0.25 * yc + 0.75 * yi) / 1.0) && near(s.y + s.h, SCR_H), "a switch: {s:?} at the weighted mean of {yc} and {yi}");

    let classic = classic();
    let fixed = state(&classic, 0).scrim.unwrap();
    for page in [0.0, 0.3, 1.0] {
        let drew = draw_in(&classic, &mut motion(Section::Info, 0.5, page), &store, OPEN_INFO, NONE, false);
        assert_eq!(scrim_of(&drew), fixed, "page {page}: a fixed scrim");
    }
}

/// Runs a test band draws: empty text (the host has no playing item) at the package's left edge, of `size`.
fn runs_of(c: &Census, size: i32) -> Vec<(f32, f32)> {
    c.1.iter().filter(|r| r.text.is_empty() && r.size == size && near(r.at.x, 136.0)).map(|r| (r.at.y, r.ink[3])).collect()
}

/// **A band that only some of the open states hold fades on that section's own appear; one that every open state
/// holds fades on the page.** The rising-tabs package with two bands added that are in no compact state: `rail`
/// (`sections`, every open state) and `chapters_rule` (`sections` and `!panel.info`, the Chapters state alone), each a
/// title pair (so the host sees its ink as two empty runs), the rail's main line in LABEL and the other's in BODY.
#[test]
fn a_band_in_only_the_open_states_fades_on_the_page_and_one_in_only_some_on_that_sections_appear() {
    let _serial = plx_base::testlock::serial();
    let _en = language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    let line = |id: &str, when: &str, size: &str| {
        let text = |bind: &str, size: &str| format!(r#"{{ "text": {{ "bind": "{bind}", "size": "{size}", "ink": "TEXT_SECONDARY", "bold": false, "max_lines": 1, "w": "fill" }} }}"#);
        format!(
            r#"{{ "id": "{id}", "when": {when}, "gap_before": "SM", "cells": [ {{ "align": "start", "vstack": {{ "gap": "XS", "w": "fill", "cells": [ {}, {} ] }} }} ] }},"#,
            text("title.kicker", "MICRO"),
            text("title.main", size)
        )
    };
    let added = format!("{}\n      {}\n      {{\n        \"id\": \"info\",", line("rail", r#""sections""#, "LABEL"), line("chapters_rule", r#"["sections", "!panel.info"]"#, "BODY"));
    let compiled = load(&edit(RISING, "{\n        \"id\": \"info\",", &added));
    let store = store();
    let (label, body) = (plx_ui::theme::size::LABEL, plx_ui::theme::size::BODY);
    let (compact, chapters, info) = (state(&compiled, 0), state(&compiled, OPEN_CHAPTERS), state(&compiled, OPEN_INFO));
    assert!(compact.bands.iter().all(|(id, _)| id != "rail" && id != "chapters_rule"));
    assert!(chapters.bands.iter().any(|(id, _)| id == "rail") && chapters.bands.iter().any(|(id, _)| id == "chapters_rule"));
    assert!(info.bands.iter().any(|(id, _)| id == "rail") && info.bands.iter().all(|(id, _)| id != "chapters_rule"));

    // at rest, each in its state: where a line stands in its band, and its ink
    let at_info = draw_in(&compiled, &mut motion(Section::Info, 1.0, 1.0), &store, OPEN_INFO, NONE, false);
    let at_chapters = draw_in(&compiled, &mut motion(Section::Chapters, 1.0, 1.0), &store, OPEN_CHAPTERS, NONE, false);
    let (rail_info, rail_chapters) = (runs_of(&at_info, label), runs_of(&at_chapters, label));
    assert_eq!((rail_info.len(), rail_chapters.len(), runs_of(&at_chapters, body).len(), runs_of(&at_info, body).len()), (1, 1, 1, 0), "the rail in both open states, the Chapters line in one: {:?}", at_info.1);
    let (rail_ink, rule_ink) = (rail_info[0].1, runs_of(&at_chapters, body)[0].1);
    assert!(rail_ink > 0.0 && rule_ink > 0.0);
    let (off_info, off_chapters) = (rail_info[0].0 - band(info, "rail").y, rail_chapters[0].0 - band(chapters, "rail").y);

    // opening Info at half: the rail (every open state) is seen at the page's half, the Chapters line is not there
    let mid = draw_in(&compiled, &mut motion(Section::Info, 0.5, 0.5), &store, OPEN_INFO, NONE, false);
    let rail = runs_of(&mid, label);
    assert_eq!(rail.len(), 1, "{:?}", mid.1);
    assert!((rail[0].1 - 0.5 * rail_ink).abs() < 0.001, "the rail fades on the page: {} of {rail_ink}", rail[0].1);
    assert!(runs_of(&mid, body).is_empty(), "the Chapters line is in no state of this journey");

    // a switch Chapters (0.25, closing) -> Info (0.75, opening) with the page up: the rail is in both, whole and
    // travelling to the mean of its two places; the Chapters line is in one and fades on Chapters' own appear
    let mut row = HudMotion::new();
    row.set_section_appear_for_test(Section::Chapters, 0.25);
    row.set_section_appear_for_test(Section::Info, 0.75);
    row.follow(OPEN_INFO);
    row.set_page_for_test(1.0);
    let sw = draw_in(&compiled, &mut row, &store, OPEN_INFO, NONE, false);
    let (rail, rule) = (runs_of(&sw, label), runs_of(&sw, body));
    assert_eq!((rail.len(), rule.len()), (1, 1), "each once: {:?}", sw.1);
    assert!((rail[0].1 - rail_ink).abs() < 0.001, "the rail is in both ends and whole: {} of {rail_ink}", rail[0].1);
    let mean = 0.25 * band(chapters, "rail").y + 0.75 * band(info, "rail").y;
    assert!((rail[0].0 - (mean + 0.25 * off_chapters + 0.75 * off_info)).abs() < 0.01, "…and stands at the weighted mean of its two places: {}", rail[0].0);
    assert!((rule[0].1 - 0.25 * rule_ink).abs() < 0.001, "the Chapters line is seen at Chapters' own appear: {} of {rule_ink}", rule[0].1);
    assert!((rule[0].0 - runs_of(&at_chapters, body)[0].0).abs() < 0.01, "…and stands where the Chapters state puts it, the only place it has");
}

/// **What the row shows while the compact HUD fades out from a stand-in.** The screen hands the discs while a
/// section is open (D7: a stand-in waits behind the page), so on the opening frame the fading row IS the discs: the
/// Up Next offer is cut at once and the discs fade out in its place. On the closing frame the compact state is the
/// real slot's at once, so the offer fades back in with the rest of the title block. (The host has no playing item,
/// so the offer's own card draws nothing; the discs are what tells the two rows apart.)
#[test]
fn a_stand_in_cuts_to_the_discs_on_the_opening_frame_and_returns_at_once_on_the_closing_one() {
    let _serial = plx_base::testlock::serial();
    let _en = language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    let (compiled, store) = (rising(), store());
    let offer = ControlSlot::UpNext(marker(MarkerKind::Credits, true));

    let rest_discs = draw_in(&compiled, &mut HudMotion::new(), &store, 0, NONE, true);
    let rest_offer = draw_in(&compiled, &mut HudMotion::new(), &store, 0, offer, true);
    assert!(has_discs(&rest_discs) && !has_discs(&rest_offer), "at rest the offer stands in the discs' place");

    // opening, half way: the screen hands the discs
    let opening = draw_in(&compiled, &mut motion(Section::Info, 0.5, 0.5), &store, OPEN_INFO, NONE, false);
    assert!(has_discs(&opening), "the discs are what fades on the opening frame");
    assert!(all_near(&title_alphas(&opening, 136.0, 40), 0.5), "…at half with the rest of the block");

    // closing, half way: the page has put the section away and hands the real slot back
    let mut closing_row = motion(Section::Info, 0.5, 0.5);
    closing_row.follow(0);
    let closing = draw_in(&compiled, &mut closing_row, &store, 0, offer, true);
    assert!(!has_discs(&closing), "the offer is back on the closing frame, the discs are not");
    assert!(all_near(&title_alphas(&closing, 136.0, 40), 0.5), "…seen at half with the rest of the block");
}

/// **The title's marquee is not reset while the page or a section moves.** The walk released the title's clock on
/// every frame the middle was withheld, so a title fading out (or in) under a section snapped back to its rest beat
/// on every frame; while anything moves it keeps its clock, and once everything has landed (or when nothing ever
/// moved) the walk releases it again, as it always did. Observed on a frame that draws no title of its own (the
/// Chapters state's), since the title's own draw releases the clock whenever it is not a paused title too wide for
/// its cell, which the host has no playing item to be.
#[test]
fn the_titles_marquee_is_released_once_the_fade_is_over_and_not_at_its_first_frame() {
    let _serial = plx_base::testlock::serial();
    let _en = language_on_this_thread_for_test(plx_platform::i18n::Preference::En);
    let (compiled, store) = (rising(), store());
    // a title that has been running for a while
    let prime = |row: &HudMotion| {
        row.title_clock.read("a long title");
        plx_machine::idle::frame_begin(0.05);
    };
    let running = |row: &HudMotion| {
        plx_machine::idle::frame_begin(0.05);
        row.title_clock.read("a long title") > 10.0
    };

    let mut row = motion(Section::Chapters, 0.5, 1.0);
    prime(&row);
    draw_in(&compiled, &mut row, &store, OPEN_CHAPTERS, NONE, false);
    assert!(running(&row), "a section half way up keeps the clock running");

    let mut row = motion(Section::Chapters, 1.0, 1.0);
    prime(&row);
    draw_in(&compiled, &mut row, &store, OPEN_CHAPTERS, NONE, false);
    assert!(!running(&row), "with everything landed and the middle withheld the clock is released, as before");

    let mut row = HudMotion::new();
    prime(&row);
    draw_in(&compiled, &mut row, &store, 0, NONE, false);
    assert!(!running(&row), "and so it is where nothing ever moved");
}
