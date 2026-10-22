//! **R15, the Library switch**, key-driven on a real `Dispatcher<AppHost>` over a real store: the
//! header's selection plate after a library switch settles.

use super::*;
use super::test_support::{World, FILM_CLUB};
use super::test_support::tick;

/// The section the STORE holds as current: the truth the page's published snapshot follows a frame
/// later.
fn store_current(r: &World) -> Option<usize> {
    let mut snapshot = plx_data::stores::browse::DirectorySnapshot::default();
    r.bridge.stores.browse.borrow_mut().capture_directory(&mut snapshot);
    snapshot.view().current()
}

/// **The selection plate stays under the library that was switched to** once the switch settles.
///
/// The page reads which library is "on view" from its own `pending` target while the grid fades out
/// and from the directory's current library afterwards (`view_section`). `flush` consumes `pending`
/// in the frame the store is told to switch, and the page's published snapshot follows the store a
/// frame later, so there is a window in which `pending` is gone and the snapshot still names the OLD
/// library. A fixture with no store never leaves that window (its directory never changes), so the
/// plate went back to the old library from the fifth frame on (lane F2). With the real store the
/// directory follows: this pins that it does, on EVERY frame, the window included.
#[test]
fn the_header_plate_stays_under_the_library_that_was_switched_to_once_the_switch_settles() {
    let mut r = World::movies_library("route-plate-after-switch", true);
    let libraries = r.library().probe_libraries();
    let names: Vec<&(u32, usize)> = libraries.iter().filter(|(_, section)| *section != usize::MAX).collect();
    assert!(names.len() >= 2, "premise: the header names two libraries, else there is no plate to move: {libraries:?}");
    let film_club = libraries.iter().find(|(_, section)| *section == FILM_CLUB).expect("premise: Film Club is named").0;
    assert_eq!(r.library().probe_plate_section(), Some(0), "premise: the plate starts under Movies");

    // Focus on the second name is not a switch.
    r.walk_header_to(film_club);
    assert_eq!(r.library().probe_plate_section(), Some(0), "focus alone never moves the plate");
    assert_eq!(store_current(&r), Some(0));

    // OK switches; watch every frame until the store has followed and a while after.
    r.step(script_key(Key::Ok, tick(r.frame + 1)));
    let mut landed_at = None;
    for frame in 0..90 {
        let (plate, store, published) = (r.library().probe_plate_section(), store_current(&r), r.bridge.browse_directory().current());
        if landed_at.is_none() && plate == Some(FILM_CLUB) { landed_at = Some(frame); }
        if landed_at.is_some() {
            assert_eq!(plate, Some(FILM_CLUB), "frame {frame}: the plate went back to {plate:?} after landing under Film Club (store {store:?}, published {published:?})");
        }
        r.step(vec![]);
    }
    assert!(landed_at.is_some(), "the OK switched the plate");
    assert_eq!(store_current(&r), Some(FILM_CLUB), "premise: the switch settled, `flush` consumed `pending` and the store followed");
    assert_eq!(r.bridge.browse_directory().current(), Some(FILM_CLUB), "the page's snapshot followed the store");
    assert_eq!(r.library().probe_plate_section(), Some(FILM_CLUB), "the plate rests under the library that was switched to");
}
