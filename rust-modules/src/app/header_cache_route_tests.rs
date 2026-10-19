//! **A header row is built when its key changes and never otherwise, on every page that wears one**
//! (the Library, its Collections and Recommended pages, the Watchlist): idle frames, a held remote
//! walking the focus along the row and up and down the page build nothing, because none of them is
//! an input the row is made of. The rule itself is `plx_ui::page_header::RowCache`/`SharedRow`
//! (unit-tested in the ui crate and per page in the screens crate); this table runs it on a real
//! `Dispatcher<AppHost>` with the real text measure and the app's own keys.
//!
//! The Library is also switched to its second library, which IS an input: the row is rebuilt for
//! it, and the frames after the switch settles build none.

use super::*;
use super::test_support::{World, FILM_CLUB};

/// Keys that move the focus and scroll the page, none of which changes what the row is made of:
/// up toward the header and the nav, along the row, and back down.
const WALK: [Key; 9] = [Key::Up, Key::Up, Key::Right, Key::Right, Key::Left, Key::Left, Key::Down, Key::Down, Key::Down];

fn quiet(name: &str, mut r: World) {
    let settled = r.header_builds();
    assert!(settled >= 1, "{name}: premise: the page's row is built");
    r.frames(90);
    assert_eq!(r.header_builds(), settled, "{name}: ninety idle frames build no row");
    for key in WALK { r.tap(key, 6); }
    r.frames(60);
    assert_eq!(r.header_builds(), settled, "{name}: walking the focus and the page builds no row");
}

#[test]
fn idle_frames_and_a_key_walk_build_no_header_row_on_any_page_that_wears_one() {
    quiet("Library, two libraries", World::movies_library("header-cache-quiet", true));
    quiet("Library, one library", World::movies_library("header-cache-quiet", false));
    quiet("Collections", World::collections("header-cache-quiet", true));
    quiet("Recommended", World::recommended("header-cache-quiet"));
    quiet("Watchlist", World::watchlist("header-cache-quiet"));
}

#[test]
fn a_library_switch_builds_the_row_and_the_frames_after_it_settle_build_none() {
    let mut r = World::movies_library("header-cache-switch", true);
    let before = r.header_builds();
    let film_club = r.library().probe_libraries().iter().find(|(_, section)| *section == FILM_CLUB).map(|(elem, _)| *elem)
        .expect("premise: Film Club names a stop");
    r.walk_header_to(film_club);
    assert_eq!(r.header_builds(), before, "focus alone on the second name builds no row");
    r.press(Key::Ok);
    r.frames(90);
    let settled = r.header_builds();
    assert!(settled > before, "the switch is an input of the row: it was rebuilt for the new library");
    r.frames(90);
    assert_eq!(r.header_builds(), settled, "and once the switch has settled the idle frames build none");
}
