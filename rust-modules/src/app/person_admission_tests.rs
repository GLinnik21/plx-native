//! What the Person store admits through the tape, observed from the `Bridge` that pumps it: the
//! person page's requests, in the order and the bytes a recording holds and a replay compares
//! (`stores::tape::admit`). The sibling of `store_admission_tests` for the Person store, written
//! the same way: each test makes ONE pump, so it does not depend on when any worker completes, and
//! it uses only signatures that did not change when the Person store moved onto the job lane
//! (`Bridge::person_run`, `Bridge::person_pump`, `assert_no_store_jobs_left_for_test`), so the same
//! file compiles and passes on both sides of that change.

use plx_data::stores::person::PersonCmd;
use plx_data::stores::tape;
use plx_plex::plex::ServerId;

use super::Bridge;

const GUID: &str = "sf06c144a";

/// Puts the tape and the registry back to their inactive state when a test ends, however it ends,
/// after any worker the pump started has finished. Declare it AFTER the serial guard, so it drops
/// first.
struct Reset;
impl Drop for Reset {
    fn drop(&mut self) {
        plx_base::task::drain_workers_for_test();
        tape::reset_for_test();
        plx_plex::plex::reset_servers_for_test();
    }
}

/// A recording armed (the tape in record mode: the launch runs and its answer is written down).
/// A controlled run asks plex.tv as nothing, so the page's two global reads are admitted without a
/// session.
fn recording() {
    plx_plex::plex::reset_servers_for_test();
    tape::init(None, false);
}

fn open(bridge: &mut Bridge, sid: ServerId) {
    assert!(bridge.person_run(PersonCmd::Open {
        sid, key: "19".into(), guid: GUID.into(), name: "Actor".into(), thumb: String::new() }));
}

/// One frame's person pump, as `run.rs` makes it.
fn pump_once(bridge: &mut Bridge) {
    let _frame = plx_base::task::FrameScope::enter();
    bridge.person_pump();
}

fn global(slot: u32) -> serde_json::Value {
    serde_json::json!({"content_resource":true,"admitted":true,
        "request":{"store":"person","slot":slot,"gen":1,"arg":[GUID],"guid":GUID}})
}

/// A person opened on a registered server asks that server's media read, then the two plex.tv
/// reads: three admissions, in ascending slot order, in one pump (fixture 12, frame 38).
#[test]
fn a_person_opened_on_a_registered_server_admits_its_three_first_reads_in_slot_order() {
    let _serial = plx_base::testlock::serial();
    let _reset = Reset;
    recording();
    let sid = plx_plex::plex::register_for_test("person-admission", "127.0.0.1", 9, "t", "cid");
    assert_eq!(sid.raw(), 0, "the slots below are server 0's");
    let client = plx_plex::plex::client_for(sid).expect("registered").instance_gen();
    let mut bridge = Bridge::for_test(|| 0);

    open(&mut bridge, sid);
    pump_once(&mut bridge);

    assert_eq!(tape::finish(), (vec![
        serde_json::json!({"content_resource":true,"admitted":true,
            "request":{"store":"person","slot":1,"gen":1,"arg":["19"],"guid":GUID,
                "local":"19","client":client,"sid":0}}),
        global(48),
        global(49),
    ], None), "the media read, then the profile and the filmography, admitted, in the recorded shape");
    bridge.assert_no_store_jobs_left_for_test();
}

/// A person whose origin server holds no client has no server read to make: the slot backs off in
/// the same pump and writes no admission, while the two plex.tv reads (which name no client) are
/// still admitted.
#[test]
fn a_person_on_a_server_with_no_client_admits_only_the_plex_tv_reads() {
    let _serial = plx_base::testlock::serial();
    let _reset = Reset;
    recording();
    let mut bridge = Bridge::for_test(|| 0);

    open(&mut bridge, ServerId::from_raw(0));
    pump_once(&mut bridge);

    assert_eq!(tape::finish(), (vec![global(48), global(49)], None),
        "no server read was admitted for a server that holds no client");
    bridge.assert_no_store_jobs_left_for_test();
}
