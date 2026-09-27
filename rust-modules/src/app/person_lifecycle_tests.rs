//! Person page lifecycle across the deferred `AppFx::Store` effect queue (review round 1, P1):
//! Enter/Uncover decide whether to emit `Open` by reading the store synchronously, but Close and
//! Open are both deferred effects applied AFTER screen event delivery in the same drain. A stack
//! eviction (`ui::containers::stack::evict`, `CAP`) delivers `Unmount` to the evicted body — which
//! queues `Close` — in the SAME `Push` drain that mounts and `Enter(Fresh)`es a new page for the
//! same person identity. The new page's `Enter(Fresh)` observes the store still holding the
//! (soon-to-be-closed) identity and skips `Open`; the queued `Close` then lands after, leaving the
//! store empty under a page that never re-requests it.

use super::*;
use super::test_support::{detail_arg, person_arg, tick};

/// Reproduces the exact sequence from the review finding: a Person A body sits at the bottom of a
/// `CAP`-full stack; the user reopens actor A's cast credit, whose `Push` evicts the bottom body
/// (queuing `Close` for the identity it still holds) in the same drain that mounts and
/// `Enter(Fresh)`s the new Person A page. The store must end up holding the person again.
#[test]
fn reopening_an_evicted_same_identity_person_leaves_the_store_holding_it() {
    let _guard = crate::testlock::serial();
    let mut d = Dispatcher::<AppHost>::new();
    let mut rig = Bridge::for_test(|| 0);
    let mut frame_no = 0u32;
    let person = person_arg("actor-a");

    show_page(&mut d, person.clone());
    frame(&mut d, &mut rig, tick(frame_no), vec![]);
    frame_no += 1;
    let bottom = d.nav.top_page().expect("Person A mounted as the first page").id;
    assert!(rig.person_view().current().is_some(),
        "the first mount's Enter(Fresh) must Open the person");

    // Fill the stack to exactly CAP live bodies, Person A still at the bottom and still live.
    for i in 0..(crate::ui::containers::stack::CAP - 1) {
        nav_push(&mut d, detail_arg(&format!("filler-{i}")));
        frame(&mut d, &mut rig, tick(frame_no), vec![]);
        frame_no += 1;
    }
    assert_eq!(d.nav.bodies().count(), crate::ui::containers::stack::CAP,
        "the stack must be exactly full before the reopening push");
    assert!(d.nav.entry(bottom).unwrap().inst.is_some(),
        "Person A's body must still be live and un-evicted right before the reopening push");

    // Reopen the SAME person identity. This Push evicts the bottom (Person A) body — delivering
    // its Unmount, and so queuing Close — in the same drain that mounts and Enter(Fresh)s the new
    // Person A page.
    nav_push(&mut d, person.clone());
    frame(&mut d, &mut rig, tick(frame_no), vec![]);

    assert!(d.nav.entry(bottom).unwrap().inst.is_none(),
        "the test must actually exercise eviction of the old Person A body, or it is not the \
         reported scenario");
    assert!(rig.person_view().current().is_some(),
        "the new Person A page's Enter(Fresh) must leave the store holding the person, not \
         blanked by the evicted body's deferred Close landing after it");
}
