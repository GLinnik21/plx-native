//! The Library's listing page and its filter-menu directories (genres, first letters) leave the
//! store as store jobs and start no worker of the store's own.

use super::*;
use crate::browse::test_support::*;
use crate::stores::browse::SectionAddress;

/// A grid step starts no worker. The loopback server refuses a connection at once, so a worker the
/// step started would have posted its (failed) answer by the time the pool has drained. The read is
/// one job on the outbox, and the page's one claim is up from the step.
#[test]
fn a_grid_step_emits_a_job_and_starts_no_worker_of_its_own() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, _sid, _client) = loopback_page_source();
    let _ = browse.pump();
    assert_eq!(browse.jobs.len(), 1, "the grid step left its first page as a job: {:?}", browse.jobs);
    assert!(adapter_fetching_for_test(&browse.adapter), "the step claimed the page fetch");
    plx_base::task::drain_workers_for_test();
    assert!(!adapter_has_page_for_test(&browse.adapter),
        "no worker of the store's own answered: the read is the application's to start");
}

/// A filter-menu step starts no worker, for either directory: each read is a job on the outbox
/// with its own claim up.
#[test]
fn a_filter_menu_step_emits_a_job_and_starts_no_worker_of_its_own() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, sid, _client) = loopback_page_source();
    browse.refill();
    let target = SectionAddress { epoch: browse.state.table_epoch(), sid, section: 1 };
    for work in [crate::stores::browse::LibraryWork::Genres, crate::stores::browse::LibraryWork::Letters] {
        assert!(browse.state.addressed_with_adapter(&browse.adapter, target, work, &browse.keys, &mut browse.jobs));
    }
    assert_eq!(browse.jobs.len(), 2, "one read per directory: {:?}", browse.jobs);
    plx_base::task::drain_workers_for_test();
    assert!(!directory_answered_for_test(&browse.adapter),
        "no worker of the store's own answered: the reads are the application's to start");
}
