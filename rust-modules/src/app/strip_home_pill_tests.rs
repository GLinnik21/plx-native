//! The Home pill lands focus in Home's content, like every other pill does.

use super::*;

/// What the Home pill queued for Home's mount, in order.
fn queued(rig: &mut Bridge) -> Vec<plx_screens::registry::HomeCmd> {
    let mut out = Vec::new();
    while let Some(command) = rig.home_commands.pop_front() { out.push(command); }
    out
}

#[test]
fn the_home_pill_seats_the_hero_instead_of_keeping_focus_on_the_strip() {
    use plx_screens::registry::HomeCmd;
    let _guard = plx_base::testlock::serial();
    let mut d = Dispatcher::<AppHost>::new();
    let mut rig = Bridge::for_test(|| 0);
    nav_home_pill(&mut d, &mut rig, None);
    assert_eq!(queued(&mut rig), vec![HomeCmd::Hero],
        "a Home pill press must leave focus in the content, not on the pill it was pressed from");
}
