//! A Related tail window read is a typed job the owner emits, not a thread the data layer starts
//! (`docs/sans-io.md`): the step takes the single-flight claim, leaves its request on the outbox and
//! starts nothing.

use super::*;
use crate::stores::metadata::{MetadataCmd, MetadataStore};
use plx_plex::plex::ServerId;

/// The sid of a server registered at a port nothing listens on: a worker that did start would
/// answer at once, with a failure, so "nothing answered" proves nothing started.
fn dead_server() -> ServerId {
    plx_plex::plex::reset_servers_for_test();
    plx_plex::plex::register_for_test("related-job", "127.0.0.1", 9, "t", "cid-related-job")
}

/// A page whose Related row is a 3-card head and a 24-card window of its tail from position 0, the
/// hub holding 100 more behind its preview: an edge ask or a seek is accepted from window `(0, 24)`.
fn page_with_tail(store: &mut MetadataStore, sid: ServerId) {
    let card = |rk: String| crate::pms::PmsMovie { sid, rk, sec: 1, ..Default::default() };
    let mut related: Vec<_> = (0..3).map(|i| card(format!("h{i}"))).collect();
    related.extend((0..24).map(|n| card(format!("t{n}"))));
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
