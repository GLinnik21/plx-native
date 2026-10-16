//! The Home hero's bookmark disc: where it sits, when it exists, what a press asks for, and that
//! the flip plays only on a real membership change of the shown title (docs/watchlist.md).

use super::*;
use plx_data::catalog::Membership;

const HEX_A: [u8; 12] = [0xa1; 12];
const HEX_B: [u8; 12] = [0xb2; 12];
const HEX_SHOW: [u8; 12] = [0x5c; 12];

/// Three heroes: rk 1 a movie (HEX_A), rk 2 a movie (HEX_B), rk 3 an episode of a show (HEX_SHOW).
fn seeded(guids: bool) -> plx_data::pms::HubsSnapshot {
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_for_test(&mut state, &adapter, 3, plx_data::pms::HubState::Ready);
    if guids {
        plx_data::pms::edit_test_item(&mut state, "1", &|m| { m.kind = 0; m.guid = Some(HEX_A); });
        plx_data::pms::edit_test_item(&mut state, "2", &|m| { m.kind = 0; m.guid = Some(HEX_B); });
        plx_data::pms::edit_test_item(&mut state, "3", &|m| { m.kind = 3; m.show_guid = Some(HEX_SHOW); });
    }
    plx_data::pms::hubs_snapshot(&state)
}

fn key(s: &HomeScreen, elem: u32) -> FocusKey<u32> {
    FocusKey { entry: s.entry, elem }
}

fn disc_rect(s: &HomeScreen, view: HubsView<'_>) -> Option<Rect> {
    s.hero_button_rect(view, 2, &plx_ui::fixture::FixtureMeasure)
}

fn tick(s: &mut HomeScreen, view: HubsView<'_>, frames: u32) {
    for frame in 0..frames {
        step(s, view, None, &ScreenEvent::Tick(Tick { ms: frame * 16, dt_us: 16_667 }));
    }
}

/// Play, the disc, Info: the disc is second, a circle one gap after Play, and Info one gap after it.
#[test]
fn the_disc_is_second_in_the_row_when_the_hero_has_a_title() {
    let _guard = plx_base::testlock::serial();
    let snapshot = seeded(true);
    let view = snapshot.view();
    let s = screen(view);
    let m = &plx_ui::fixture::FixtureMeasure;
    let play = s.hero_button_rect(view, 0, m).unwrap();
    let disc = disc_rect(&s, view).expect("a movie hero with a guid draws the disc");
    let info = s.hero_button_rect(view, 1, m).unwrap();
    assert_eq!((disc.w, disc.h), (HERO_CTRL_D, HERO_CTRL_D));
    assert_eq!(disc.x, play.x + play.w + HERO_CTRL_GAP);
    assert_eq!(info.x, disc.x + disc.w + HERO_CTRL_GAP);
    assert_eq!(s.hero_len(view), 3);
    assert_eq!(s.hero_slots(view), ([0, 2, 1], 3));
    let mut groups = Vec::new();
    Focusable::<TestHost>::groups(&s, &cx(view, None), &mut groups);
    assert_eq!(groups[0].len, 3);
    assert_eq!(groups[0].extent.x + groups[0].extent.w, info.x + info.w, "the extent ends at Info");
}

/// An item with no portable id has no disc: the row is the shipped Play and Info.
#[test]
fn a_hero_without_a_catalog_id_has_no_disc() {
    let _guard = plx_base::testlock::serial();
    let snapshot = seeded(false);
    let view = snapshot.view();
    let s = screen(view);
    assert!(disc_rect(&s, view).is_none());
    assert_eq!(s.hero_slots(view), ([0, 1, 0], 2));
    assert_eq!(Focusable::<TestHost>::group_of(&s, &HERO_WATCHLIST_ELEM, &cx(view, None)), None);
}

#[test]
fn left_and_right_walk_the_row_in_drawn_order() {
    let _guard = plx_base::testlock::serial();
    let snapshot = seeded(true);
    let view = snapshot.view();
    let s = screen(view);
    let c = cx(view, None);
    let go = |from: u32, dir| match Focusable::<TestHost>::neighbour(&s, key(&s, from), dir, &c) {
        Step::Move(k) => Some(k.elem),
        _ => None,
    };
    assert_eq!(go(HERO_PLAY_ELEM, Dir::Right), Some(HERO_WATCHLIST_ELEM));
    assert_eq!(go(HERO_WATCHLIST_ELEM, Dir::Right), Some(HERO_INFO_ELEM));
    assert_eq!(go(HERO_INFO_ELEM, Dir::Left), Some(HERO_WATCHLIST_ELEM));
    assert_eq!(go(HERO_WATCHLIST_ELEM, Dir::Left), Some(HERO_PLAY_ELEM));
    assert_eq!(go(HERO_INFO_ELEM, Dir::Right), None);
}

fn item_menu_act(out: &[Stamped<TestHost>]) -> Option<crate::registry::ItemMenuAction> {
    out.iter().find_map(|st| match &st.fx {
        Fx::App(AppFx::ItemMenu(req)) => Some(req.act.clone()),
        _ => None,
    })
}

/// A press asks the app for the item menu's own write: Add from Off and Unknown, Remove from On.
#[test]
fn a_press_asks_for_add_or_remove() {
    use crate::registry::ItemMenuAction as Action;
    let _guard = plx_base::testlock::serial();
    let snapshot = seeded(true);
    let view = snapshot.view();
    let mut s = screen(view);
    let (_, out, _) = step(&mut s, view, None, &ScreenEvent::Activate(HERO_WATCHLIST_ELEM));
    assert_eq!(item_menu_act(&out), Some(Action::WatchlistAdd { hex: HEX_A }), "unknown wears the Add face");
    s.set_watchlist(HEX_A, Membership::Off);
    let (_, out, _) = step(&mut s, view, None, &ScreenEvent::Activate(HERO_WATCHLIST_ELEM));
    assert_eq!(item_menu_act(&out), Some(Action::WatchlistAdd { hex: HEX_A }));
    assert!(!has_home(&out, |_| true), "the disc opens no page and plays nothing");
    s.set_watchlist(HEX_A, Membership::On);
    let (_, out, _) = step(&mut s, view, None, &ScreenEvent::Activate(HERO_WATCHLIST_ELEM));
    assert_eq!(item_menu_act(&out), Some(Action::WatchlistRemove(format!("plex://movie/{}", "a1".repeat(12)))));
}

/// An episode hero acts on its show.
#[test]
fn an_episode_hero_adds_its_show() {
    use crate::registry::ItemMenuAction as Action;
    let _guard = plx_base::testlock::serial();
    let snapshot = seeded(true);
    let view = snapshot.view();
    let mut s = screen(view);
    step(&mut s, view, None, &ScreenEvent::App(AppMsg::Home(HomeCmd::PinHero(2))));
    tick(&mut s, view, 1);
    assert_eq!(s.watchlist_hex(), Some(HEX_SHOW));
    let (_, out, _) = step(&mut s, view, None, &ScreenEvent::Activate(HERO_WATCHLIST_ELEM));
    assert_eq!(item_menu_act(&out), Some(Action::WatchlistAdd { hex: HEX_SHOW }));
}

/// The first answer for an already-listed title snaps the fill: nothing flips on open.
#[test]
fn an_already_listed_title_does_not_flip_on_open() {
    let _guard = plx_base::testlock::serial();
    let snapshot = seeded(true);
    let view = snapshot.view();
    let mut s = screen(view);
    tick(&mut s, view, 1);
    assert_eq!(s.watchlist_hex(), Some(HEX_A));
    s.set_watchlist(HEX_A, Membership::On);
    assert_eq!(s.disc.progress(HEX_A), 1.0);
    tick(&mut s, view, 30);
    assert_eq!((s.disc.flip.pos, s.disc.flip.vel), (1.0, 0.0));
    // The list landing after the page drew (Unknown, then On) snaps too.
    let mut late = screen(view);
    late.set_watchlist(HEX_A, Membership::Unknown);
    late.set_watchlist(HEX_A, Membership::On);
    assert_eq!(late.disc.progress(HEX_A), 1.0);
}

/// Rotating to another title swaps the face with no animation; the title that left keeps its face.
#[test]
fn rotating_the_hero_does_not_flip() {
    let _guard = plx_base::testlock::serial();
    let snapshot = seeded(true);
    let view = snapshot.view();
    let mut s = screen(view);
    tick(&mut s, view, 1);
    s.set_watchlist(HEX_A, Membership::On);
    assert!(s.flip(view, 1));
    tick(&mut s, view, 1);
    assert_eq!(s.watchlist_hex(), Some(HEX_B));
    s.set_watchlist(HEX_B, Membership::Off);
    assert_eq!(s.disc.progress(HEX_B), 0.0);
    assert_eq!(s.disc.progress(HEX_A), 1.0, "the page sliding out still wears its filled face");
    tick(&mut s, view, 30);
    assert_eq!((s.disc.flip.pos, s.disc.flip.vel), (0.0, 0.0));
    // And back onto a listed title: snapped, not flipped.
    s.set_watchlist(HEX_A, Membership::On);
    assert_eq!((s.disc.flip.pos, s.disc.flip.vel), (1.0, 0.0));
}

/// A real change on the shown title plays the flip, and settles exactly on the new face.
#[test]
fn a_membership_change_of_the_shown_title_flips() {
    let _guard = plx_base::testlock::serial();
    let snapshot = seeded(true);
    let view = snapshot.view();
    let mut s = screen(view);
    tick(&mut s, view, 1);
    s.set_watchlist(HEX_A, Membership::Off);
    tick(&mut s, view, 30);
    s.set_watchlist(HEX_A, Membership::On);
    assert_eq!(s.disc.flip.pos, 0.0, "the flip starts at the old face");
    tick(&mut s, view, 4);
    assert!(s.disc.flip.pos > 0.0 && s.disc.flip.pos < 1.0, "mid-flip after a few frames: {}", s.disc.flip.pos);
    let (_, mid) = plx_appkit::watchlist_disc::paint(s.disc.progress(HEX_A));
    assert!(mid.is_some(), "a flip in flight layers the fill");
    tick(&mut s, view, 120);
    assert_eq!((s.disc.flip.pos, s.disc.flip.vel), (1.0, 0.0));
}

/// A focus resting on the disc when the hero rotates to a title without one returns to Play.
#[test]
fn a_vanished_disc_returns_focus_to_play() {
    let _guard = plx_base::testlock::serial();
    // Only rk 1 carries an id; the hero rotates onto rk 2 with focus on the disc.
    let mut state = plx_data::pms::PmsState::default();
    let adapter = std::sync::Arc::new(plx_data::pms::PmsAdapter::default());
    plx_data::pms::seed_for_test(&mut state, &adapter, 3, plx_data::pms::HubState::Ready);
    plx_data::pms::edit_test_item(&mut state, "1", &|m| { m.kind = 0; m.guid = Some(HEX_A); });
    let mixed = plx_data::pms::hubs_snapshot(&state);
    let mview = mixed.view();
    let mut m = screen(mview);
    assert!(m.flip(mview, 1));
    let want = key(&m, HERO_WATCHLIST_ELEM);
    assert_eq!(Focusable::<TestHost>::reconcile(&m, want, &cx(mview, Some(want))).elem, HERO_PLAY_ELEM);
}
