//! A detail request is a typed job the owner emits, not a thread the data layer starts
//! (`docs/sans-io.md`): what a request reserves, what the job says, and when the reservation
//! comes back.

use super::*;
use crate::stores::metadata::{MetadataCmd, MetadataStore};

fn request(store: &mut MetadataStore, sid: plx_plex::plex::ServerId, rk: &str) {
    assert!(store.run(MetadataCmd::RequestDetail { sid, rk: rk.to_string() }));
}

/// A request that a newer one supersedes keeps its landing reservation until ITS fetch answers.
/// The device has always behaved so: a worker holds its slot until it returns, and an answer
/// that arrives for a superseded request is discarded by the generation check while handing the
/// slot back. The test lane used to answer a superseded fetch inline, inside the new request, so
/// the very next pump drained that answer and the slot came back before any fetch had returned:
/// no test could see the second reservation outlive a pump, or the stale answer arrive.
#[test]
fn a_superseded_detail_request_keeps_its_reservation_until_answered() {
    let _serial = plx_base::testlock::serial();
    let sid = plx_plex::plex::ServerId::UNSET;
    let mut store = MetadataStore::default();
    let to = detail_addr(1).to;
    let inflight = |store: &MetadataStore| store.adapter_ref().detail_landing_ref().inflight(to);

    request(&mut store, sid, "old");
    request(&mut store, sid, "new");
    assert_eq!(inflight(&store), 2, "the superseded request still owes an answer, so it still holds its slot");

    // No fetch has returned, so a pump has nothing to drain and frees nothing.
    assert!(!store.pump_detail(), "nothing has landed");
    assert_eq!(inflight(&store), 2, "a pump with no answer in hand does not free the superseded slot");

    // The stale answer arrives: its slot comes back, nothing is installed, the page still waits.
    land_detail(store.adapter_ref(), sid, "old", 1, None);
    assert!(!store.pump_detail(), "a superseded answer installs nothing");
    assert_eq!(inflight(&store), 1);
    assert!(store.view().detail_loading(), "the current request is still out");

    // The current answer arrives: the page settles, with exactly one notice.
    let before = store.gen();
    land_detail(store.adapter_ref(), sid, "new", 2, Some(Detail::default()));
    assert!(store.pump_detail(), "the current answer settles the page");
    assert_eq!(store.gen(), before + 1, "one answer, one notice");
    assert_eq!(inflight(&store), 0);
    assert!(!store.view().detail_loading());
}

/// The contract the app's runner reads: one accepted request is one job, addressed by its
/// generation, carrying the server, the item and the season to open on, and the port of the
/// owner that asked. Nothing else is emitted, and the outbox is empty once it is taken.
#[test]
fn a_detail_request_emits_one_job_carrying_everything_the_worker_needs() {
    use crate::stores::StoreJob;
    use plx_machine::job::Job;
    let _serial = plx_base::testlock::serial();
    let sid = plx_plex::plex::ServerId::UNSET;
    let mut store = MetadataStore::default();
    assert!(!store.has_jobs());

    request(&mut store, sid, "42");
    assert!(store.has_jobs());
    plx_machine::assert_fx!(store.take_jobs(), [
        StoreJob::Detail(
            Job { addr: detail_addr(1), req: DetailReq { sid, rk: "42".into(), prefer: None } },
            store.detail_port(),
        ),
    ]);
    assert!(!store.has_jobs(), "taking the jobs empties the outbox");
    plx_machine::assert_fx!(store.take_jobs(), []);

    request(&mut store, sid, "43");
    request(&mut store, sid, "44");
    plx_machine::assert_fx!(store.take_jobs(), [
        StoreJob::Detail(
            Job { addr: detail_addr(2), req: DetailReq { sid, rk: "43".into(), prefer: None } },
            store.detail_port(),
        ),
        StoreJob::Detail(
            Job { addr: detail_addr(3), req: DetailReq { sid, rk: "44".into(), prefer: None } },
            store.detail_port(),
        ),
    ]);

    // The exactly-once property on the refusal side: a request the landing will not admit (four
    // answers already owed, none returned) is settled synchronously and emits NO job, so no runner
    // can answer it a second time. The fourth request is the last the landing admits.
    request(&mut store, sid, "45");
    request(&mut store, sid, "46");
    plx_machine::assert_fx!(store.take_jobs(), [
        StoreJob::Detail(
            Job { addr: detail_addr(4), req: DetailReq { sid, rk: "45".into(), prefer: None } },
            store.detail_port(),
        ),
    ]);
    assert_eq!(store.view().detail_request_status(sid, "46"), Some(false),
        "the refused request settled in the step that refused it");
    assert!(!store.view().detail_loading());
}

/// The season the page already shows travels in the job: the worker must not read the store, so
/// `prefer` is captured when the request is made, and only for the item the page is showing.
#[test]
fn a_detail_job_captures_the_season_the_page_shows_for_the_same_item_only() {
    use crate::stores::StoreJob;
    use plx_machine::job::Job;
    let _serial = plx_base::testlock::serial();
    let sid = plx_plex::plex::ServerId::UNSET;
    let mut store = MetadataStore::default();
    let season = |index| Season {
        rk: format!("s{index}"), index, title: format!("Season {index}"), leaf_count: 0, viewed_leaf_count: 0,
    };
    set_current_for_test(store.state_mut(), Some(Detail {
        sid, rk: "show".into(), is_show: true, seasons: vec![season(1), season(3)], cur_season: 1,
        ..Default::default()
    }));

    request(&mut store, sid, "show");
    request(&mut store, sid, "another");
    plx_machine::assert_fx!(store.take_jobs(), [
        StoreJob::Detail(
            Job { addr: detail_addr(1), req: DetailReq { sid, rk: "show".into(), prefer: Some(3) } },
            store.detail_port(),
        ),
        StoreJob::Detail(
            Job { addr: detail_addr(2), req: DetailReq { sid, rk: "another".into(), prefer: None } },
            store.detail_port(),
        ),
    ]);
}

/// A job's port is captured when the job is emitted. A `Reset` between the request and the
/// runner rotates the owner's adapter, and the job still answers into the retired one, where
/// nothing is pumped, never into the replacement's landing.
#[test]
fn a_job_emitted_before_a_reset_answers_into_the_retired_adapter() {
    use crate::stores::StoreJob;
    use plx_machine::job::Port;
    let _serial = plx_base::testlock::serial();
    let sid = plx_plex::plex::ServerId::UNSET;
    let mut store = MetadataStore::default();

    request(&mut store, sid, "42");
    store.run(MetadataCmd::Reset);
    let mut jobs = store.take_jobs();
    assert_eq!(jobs.len(), 1, "a reset does not drop an emitted job");
    let StoreJob::Detail(job, port) = jobs.remove(0) else { panic!("a detail request emits a detail job") };
    assert_ne!(port, store.detail_port(), "the job holds the retired adapter's port");

    port.done(job.addr, ((sid, "42".into()), Some(Detail::default())));
    assert!(!store.pump_detail(), "the replacement's landing never sees the answer");
    assert!(!store.view().detail_loading());
}
