//! A Related tail window read is a typed job the owner emits, not a thread the data layer starts
//! (`docs/sans-io.md`): the step takes the single-flight claim, leaves its request on the outbox and
//! starts nothing.

use super::*;
use crate::stores::metadata::{MetadataCmd, MetadataStore};
use crate::stores::paging::{PageInfo, RowState};
use crate::stores::StoreJob;
use plx_machine::job::{Job, Port};
use plx_plex::plex::ServerId;

/// The sid of a server registered at a port nothing listens on: a worker that did start would
/// answer at once, with a failure, so "nothing answered" proves nothing started.
fn dead_server() -> ServerId {
    plx_plex::plex::reset_servers_for_test();
    plx_plex::plex::register_for_test("related-job", "127.0.0.1", 9, "t", "cid-related-job")
}

fn card(sid: ServerId, rk: String) -> crate::pms::PmsMovie {
    crate::pms::PmsMovie { sid, rk, sec: 1, ..Default::default() }
}

/// A page whose Related row is a 3-card head and a 24-card window of its tail from position 0, the
/// hub holding 100 more behind its preview: an edge ask or a seek is accepted from window `(0, 24)`.
fn page_with_tail(store: &mut MetadataStore, sid: ServerId) {
    let mut related: Vec<_> = (0..3).map(|i| card(sid, format!("h{i}"))).collect();
    related.extend((0..24).map(|n| card(sid, format!("t{n}"))));
    set_current_for_test(store.state_mut(), Some(Detail {
        sid,
        rk: "movie".into(),
        kind: "movie".into(),
        related,
        related_tail: RelatedTail {
            hubs: vec![TailHub { key: "/library/metadata/1/similar".into(), preview: 3, len: Some(100) }],
            head: 3,
            positions: (0..24).collect(),
            offset: 0,
            end: 24,
            total: 100,
            more: true,
            ..Default::default()
        },
        ..Default::default()
    }));
}

/// The read is out and nothing has answered it: the claim is up, the window read has not landed.
fn read_is_out_and_unanswered(store: &MetadataStore) {
    let adapter = store.adapter_for_test();
    let rel = adapter.rel_pages.lock().unwrap();
    assert!(rel.inflight, "the single-flight claim is up for the read that is out");
    assert!(rel.landed.is_empty(), "no worker ran: nothing was answered");
}

/// An edge ask takes the claim and leaves one request on the outbox: it starts no worker of its own.
#[test]
fn a_related_edge_ask_leaves_its_request_on_the_outbox_and_starts_no_worker() {
    let _serial = plx_base::testlock::serial();
    let sid = dead_server();
    let mut store = MetadataStore::default();
    page_with_tail(&mut store, sid);
    assert!(!store.has_jobs());

    assert!(store.run(MetadataCmd::WantRelated { before: false, seen: (0, 24) }), "the ask is taken");
    plx_base::task::drain_workers_for_test();

    assert!(store.has_jobs(), "the ask emitted its request");
    assert_eq!(store.take_jobs().len(), 1);
    read_is_out_and_unanswered(&store);
}

/// A seek is the same read opened at a tail position: one request on the outbox, no worker.
#[test]
fn a_related_seek_leaves_its_request_on_the_outbox_and_starts_no_worker() {
    let _serial = plx_base::testlock::serial();
    let sid = dead_server();
    let mut store = MetadataStore::default();
    page_with_tail(&mut store, sid);
    assert!(!store.has_jobs());

    assert!(store.run(MetadataCmd::SeekRelated { at: 60, seen: (0, 24) }), "the seek is taken");
    plx_base::task::drain_workers_for_test();

    assert!(store.has_jobs(), "the seek emitted its request");
    assert_eq!(store.take_jobs().len(), 1);
    read_is_out_and_unanswered(&store);
}

/// The window the page holds: tail positions 0 to 24 of a hub with 100 behind its preview.
const FROM: (usize, usize) = (0, 24);

/// An edge ask taken, and the one job it left: its address, its request and its port.
fn related_job(store: &mut MetadataStore) -> (plx_machine::machine::Addr, MetaRelatedReq, MetaRelatedPort) {
    let mut jobs = store.take_jobs();
    assert_eq!(jobs.len(), 1, "one ask, one request");
    let StoreJob::MetaRelated(job, port) = jobs.remove(0) else { panic!("a Related ask emits a MetaRelated job") };
    (job.addr, job.req, port)
}

fn ask_next(store: &mut MetadataStore) -> bool {
    store.run(MetadataCmd::WantRelated { before: false, seen: FROM })
}

/// The window a read would hand back: no cards, the same place on the listing.
fn an_answer() -> Option<RelatedWindow> {
    let info = PageInfo { offset: 12, end: 36, total: 100, more: true, unstable: false };
    Some((Vec::new(), info, Vec::new(), RowState::default()))
}

fn claim(store: &MetadataStore) -> (bool, bool) {
    related_claim_for_test(&store.adapter_for_test())
}

/// The job an edge ask leaves is the whole of what the read needs, byte for byte: the ids, the
/// window it starts from with its cards, the keys the tail must not show (the head's), the hubs, the
/// ledger the window carries and the view-state stamp taken at the ask. Its port reaches the adapter
/// the ask was made from.
#[test]
fn an_edge_ask_emits_exactly_the_request_the_read_needs() {
    let _serial = plx_base::testlock::serial();
    let sid = dead_server();
    let mut store = MetadataStore::default();
    page_with_tail(&mut store, sid);
    assert!(ask_next(&mut store));
    let adapter = store.adapter_for_test();
    let stamp = store.state().ledger.stamp();
    let rows = (0..24).map(|n| (n, std::sync::Arc::new(card(sid, format!("t{n}"))))).collect();

    plx_machine::assert_fx!(store.take_jobs(), [StoreJob::MetaRelated(
        Job {
            addr: related_addr(1),
            req: MetaRelatedReq {
                sid,
                rk: "movie".into(),
                ask: RelatedAsk::Edge { before: false },
                info: PageInfo { offset: 0, end: 24, total: 100, more: true, unstable: false },
                rows,
                skip: ["h0", "h1", "h2"].into_iter().map(String::from).collect(),
                hubs: vec![TailHub { key: "/library/metadata/1/similar".into(), preview: 3, len: Some(100) }],
                row: RowState::default(),
                stamp,
            },
        },
        MetaRelatedPort::of(&adapter),
    )]);
}

/// A seek opens its window on the listing alone: it carries no cards and an empty ledger, and names
/// the tail position.
#[test]
fn a_seek_emits_a_request_with_no_window_to_carry() {
    let _serial = plx_base::testlock::serial();
    let sid = dead_server();
    let mut store = MetadataStore::default();
    page_with_tail(&mut store, sid);
    assert!(store.run(MetadataCmd::SeekRelated { at: 60, seen: FROM }));

    let (addr, req, _) = related_job(&mut store);

    assert_eq!(addr, related_addr(1));
    assert_eq!(req.ask, RelatedAsk::Seek(60));
    assert_eq!((req.info.offset, req.info.end), FROM, "the landing is checked against the window it replaces");
    assert!(req.rows.is_empty(), "a seek carries no cards");
    assert_eq!(req.row, RowState::default());
}

/// A job nobody started: the refusal gives the single-flight claim back in the call that refuses
/// it, which is where the inline spawn's `!spawned` arm gave it back, and the next edge asks again
/// (while the job was out it would not).
#[test]
fn a_refused_related_job_gives_the_claim_back_and_the_next_edge_asks_again() {
    let _serial = plx_base::testlock::serial();
    let mut store = MetadataStore::default();
    page_with_tail(&mut store, dead_server());
    assert!(ask_next(&mut store));
    let (addr, _, port) = related_job(&mut store);
    assert_eq!(claim(&store), (true, false));
    assert!(!ask_next(&mut store), "the read is out: it is not asked for twice");
    assert!(!store.has_jobs());

    port.refused(addr);

    assert_eq!(claim(&store), (false, false), "the refusal gave the claim back in its own call");
    assert!(ask_next(&mut store), "the next edge asks again");
    let (again, ..) = related_job(&mut store);
    assert_eq!(again, related_addr(2), "and it is a new job");
}

/// The reader withdrew the read, then the launch was refused: no read is out for the flag to
/// discard, so it is cleared with the claim and the next read lands.
#[test]
fn a_withdrawn_then_refused_related_job_leaves_no_stale_withdrawal() {
    let _serial = plx_base::testlock::serial();
    let sid = dead_server();
    let mut store = MetadataStore::default();
    page_with_tail(&mut store, sid);
    assert!(ask_next(&mut store));
    let (addr, _, port) = related_job(&mut store);
    store.run(MetadataCmd::CancelRelated);
    assert_eq!(claim(&store), (true, true), "withdrawn while the job is out");

    port.refused(addr);

    assert_eq!(claim(&store), (false, false), "the refusal took the flag with the claim");
    assert!(ask_next(&mut store));
    let (addr, req, port) = related_job(&mut store);
    finish_related_with(&port, addr, plx_plex::plex::client_for(sid), req, |_, _| an_answer());
    let rel = store.adapter_for_test();
    let rel = rel.rel_pages.lock().unwrap();
    assert_eq!(rel.landed.len(), 1, "the next read was not discarded on a stale flag");
    assert!(rel.landed[0].window.is_some());
}

/// The reader withdrew the read while it was out: its answer is dropped when it arrives, the claim
/// comes back with it, and the next edge asks again.
#[test]
fn a_withdrawn_related_job_answers_nothing_and_the_next_edge_asks_again() {
    let _serial = plx_base::testlock::serial();
    let sid = dead_server();
    let mut store = MetadataStore::default();
    page_with_tail(&mut store, sid);
    assert!(ask_next(&mut store));
    let (addr, req, port) = related_job(&mut store);
    store.run(MetadataCmd::CancelRelated);

    finish_related_with(&port, addr, plx_plex::plex::client_for(sid), req, |_, _| an_answer());

    assert_eq!(claim(&store), (false, false), "the answer gave the claim back and spent the flag");
    assert!(!store.pump_related(), "the withdrawn answer installs nothing");
    assert!(ask_next(&mut store), "the next edge asks again");
}

/// A server the registry holds no client for by the launch: the body answers the FAILURE without a
/// GET, which gives the claim back and installs nothing.
#[test]
fn a_related_job_whose_server_has_no_client_answers_the_failure() {
    let _serial = plx_base::testlock::serial();
    let mut store = MetadataStore::default();
    page_with_tail(&mut store, dead_server());
    assert!(ask_next(&mut store));
    let (addr, req, port) = related_job(&mut store);

    finish_related_job(&port, addr, None, req);

    assert_eq!(claim(&store), (false, false), "answered: the claim came back");
    let adapter = store.adapter_for_test();
    assert_eq!(adapter.rel_pages.lock().unwrap().landed.len(), 1, "one answer landed: a refusal lands none");
    assert!(adapter.rel_pages.lock().unwrap().landed.iter().all(|l| l.window.is_none()), "a failure, not a window");
    assert!(!store.pump_related());
    assert!(ask_next(&mut store), "and the next edge asks again");
}

/// A read that panics lands as a FAILURE, not as nothing: the answer is posted outside the guard, so
/// the claim does not stay up for a read that is gone.
#[test]
fn a_panicking_related_read_lands_as_a_failure_and_releases_the_claim() {
    let _serial = plx_base::testlock::serial();
    let sid = dead_server();
    let mut store = MetadataStore::default();
    page_with_tail(&mut store, sid);
    assert!(ask_next(&mut store));
    let (addr, req, port) = related_job(&mut store);

    finish_related_with(&port, addr, plx_plex::plex::client_for(sid), req, |_, _| panic!("a broken parser"));

    assert_eq!(claim(&store), (false, false), "the failure released the claim");
    let adapter = store.adapter_for_test();
    let rel = adapter.rel_pages.lock().unwrap();
    assert_eq!(rel.landed.len(), 1);
    assert!(rel.landed[0].window.is_none(), "landed as a failure");
}

/// A job's port is captured when the job is emitted. A `Reset` between the ask and the runner
/// rotates the owner's adapter, and the job still answers into the retired one, where nothing is
/// pumped, never into the replacement's mailbox.
#[test]
fn a_related_job_emitted_before_a_reset_answers_into_the_retired_adapter() {
    let _serial = plx_base::testlock::serial();
    let sid = dead_server();
    let mut store = MetadataStore::default();
    page_with_tail(&mut store, sid);
    assert!(ask_next(&mut store));
    store.run(MetadataCmd::Reset);
    let (addr, req, port) = related_job(&mut store);
    assert!(port != MetaRelatedPort::of(&store.adapter_for_test()), "the job holds the retired adapter's port");

    finish_related_with(&port, addr, plx_plex::plex::client_for(sid), req, |_, _| an_answer());

    assert!(!store.pump_related(), "the replacement's mailbox never sees the answer");
    assert!(store.adapter_for_test().rel_pages.lock().unwrap().landed.is_empty());
    assert_eq!(claim(&store), (false, false));
}
