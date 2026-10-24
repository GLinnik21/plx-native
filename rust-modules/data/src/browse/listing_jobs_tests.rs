//! The guard that stays in the body of a listing read: a read that panics lands as the failure its
//! kind already had, and the landing outside the guard is the store's own.

use super::*;
use crate::browse::test_support::*;
use crate::browse::SecFetch;
use crate::stores::StoreJob;

/// The page job the steps have left, taken off the outbox.
#[track_caller]
fn the_page_job(browse: &mut TestBrowse) -> (plx_machine::job::Job<BrowsePageReq>, BrowsePagePort) {
    let mut jobs = std::mem::take(&mut browse.jobs);
    assert_eq!(jobs.len(), 1, "exactly one job is on the outbox: {jobs:?}");
    match jobs.remove(0) {
        StoreJob::BrowsePage(job, port) => (job, port),
        other => panic!("a page job, not {other:?}"),
    }
}

/// A page body that panics answers a FAILED page (`total < 0`, never an empty one): the landing
/// marks the listing failed and arms the retry countdown, exactly as a read that failed does, and
/// the claim the step raised is lowered by the take.
#[test]
fn a_panicking_page_body_lands_as_a_failed_page_and_arms_the_retry() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, _sid, client) = loopback_page_source();
    let _ = browse.pump();
    let (job, port) = the_page_job(&mut browse);
    assert!(browse.adapter.page.busy(), "premise: the step raised the page claim");

    finish_page_with(&port, job.addr, client, job.req, |_, _, _, _, _| panic!("the body panicked"));

    assert!(browse.adapter.page.has_mail(), "the answer was posted, outside the guard");
    let outcome = browse.pump();
    assert!(outcome.changed, "the failed page was landed");
    assert_eq!(browse.state.states()[0].fetch, SecFetch::Failed, "a panic is a failed fetch");
    assert!(browse.state.states()[0].total < 0, "…and the listing keeps no total it did not get");
    assert_eq!(browse.state.retry_cd, 120, "…and the retry countdown is armed");
    assert!(!browse.adapter.page.busy(), "the take lowered the claim");
    assert!(browse.jobs.is_empty(), "no new page is asked for while the countdown runs: {:?}", browse.jobs);
}

/// A directory body that panics answers an empty list, as a read that found nothing does: the
/// landing marks the list fetched (the menu is not left waiting) and the claim goes down.
#[test]
fn a_panicking_directory_body_lands_as_an_empty_list() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, sid, client) = loopback_page_source();
    browse.refill();
    let target = crate::stores::browse::SectionAddress { epoch: browse.state.table_epoch(), sid, section: 1 };
    assert!(browse.state.addressed_with_adapter(&browse.adapter, target,
        crate::stores::browse::LibraryWork::Genres, &browse.keys, &mut browse.jobs));
    let (job, port) = match browse.jobs.remove(0) {
        StoreJob::BrowseDirectory(job, port) => (job, port),
        other => panic!("a directory job, not {other:?}"),
    };
    assert!(browse.adapter.genres.busy(), "premise: the step raised the genre claim");

    finish_directory_with(&port, job.addr, client, job.req, || panic!("the body panicked"));

    assert!(browse.adapter.genres.has_mail(), "the answer was posted, outside the guard");
    let _ = browse.pump();
    let state = &browse.state.states()[0];
    assert!(state.genres_done, "an empty list is a landed list");
    assert!(state.genres.is_empty());
    assert!(!browse.adapter.genres.busy(), "the take lowered the claim");
}
