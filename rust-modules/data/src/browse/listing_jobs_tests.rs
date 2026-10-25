//! The guard that stays in the body of a listing read: a read that panics lands as the failure its
//! kind already had, and the landing outside the guard is the store's own. And the settle of a read
//! the runner refused: the next step finds the claim back and the refusal's record, consumes it, and
//! asks again with the registry's key as of THAT step.

use super::*;
use crate::browse::test_support::*;
use crate::browse::SecFetch;
use crate::stores::browse::{LibraryWork, SectionAddress};
use crate::stores::StoreJob;
use plx_plex::plex::{ClientKey, ClientKeys};

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

/// The directory job the steps have left, taken off the outbox.
#[track_caller]
fn the_directory_job(browse: &mut TestBrowse) -> (plx_machine::job::Job<BrowseDirectoryReq>, BrowseDirectoryPort) {
    let mut jobs = std::mem::take(&mut browse.jobs);
    assert_eq!(jobs.len(), 1, "exactly one job is on the outbox: {jobs:?}");
    match jobs.remove(0) {
        StoreJob::BrowseDirectory(job, port) => (job, port),
        other => panic!("a directory job, not {other:?}"),
    }
}

/// The menu's tick for `work` (`Genres` or `Letters`) on the registry as of now.
fn tick_directory(browse: &mut TestBrowse, sid: plx_plex::plex::ServerId, work: LibraryWork) {
    browse.refill();
    let target = SectionAddress { epoch: browse.state.table_epoch(), sid, section: 1 };
    assert!(browse.state.addressed_with_adapter(&browse.adapter, target, work, &browse.keys, &mut browse.jobs));
}

/// The tick on a registry that holds no client, as the step the refused job's settle runs in.
fn tick_directory_without_keys(browse: &mut TestBrowse, sid: plx_plex::plex::ServerId, work: LibraryWork)
    -> Vec<StoreJob> {
    let target = SectionAddress { epoch: browse.state.table_epoch(), sid, section: 1 };
    let mut jobs = Vec::new();
    assert!(browse.state.addressed_with_adapter(&browse.adapter, target, work, &ClientKeys::default(), &mut jobs));
    jobs
}

/// Is the refusal of directory request `id` still on the adapter, unconsumed? (Consumes it if so.)
fn directory_refusal_pending(browse: &TestBrowse, which: Directory, id: u32) -> bool {
    match which {
        Directory::Genres => browse.adapter.genres.take_refused(id),
        Directory::Letters => browse.adapter.letters.take_refused(id),
    }
}

/// A page job whose key moved by the time the runner launched it is refused. Nothing is recorded:
/// the store's next step settles it (the claim is down, the refusal's record consumed) and asks
/// nothing while the registry holds no client for the server. The step after, with the client back
/// under a new key, asks again with THAT key.
#[test]
fn a_refused_page_job_is_settled_by_the_next_step_and_the_one_after_asks_again_with_the_new_key() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, sid, client) = loopback_page_source();
    let first = ClientKey::of(client);
    browse.maybe_spawn();
    let (job, port) = the_page_job(&mut browse);
    assert_eq!(job.req.server(), (sid, first), "premise: the first read asked with the first key");
    assert!(browse.adapter.page.busy(), "premise: the step raised the page claim");

    port.refused(job.addr); // the runner found the registry's client moved at the launch
    assert!(!browse.adapter.page.busy(), "the refusal gave the claim back in the call that refused");
    assert!(browse.state.states()[0].fetch == SecFetch::Loading,
        "…and nothing is settled until the store's next step");

    // The next step finds the registry holding no client for the server: it settles the refusal and
    // has nothing to ask with.
    let mut jobs = Vec::new();
    browse.state.maybe_spawn_owned(&browse.adapter, &ClientKeys::default(), &mut jobs);
    assert!(jobs.is_empty(), "no client, no read: {jobs:?}");
    assert!(!browse.adapter.page.busy(), "no claim is left up without a job in flight");
    assert!(!browse.adapter.page.take_refused(job.addr.req.0), "the refusal is consumed");

    // The client is back, under a key that is not the first.
    client.set_token("new");
    assert_ne!(ClientKey::of(client), first, "premise: the key moved");
    browse.maybe_spawn();
    let (job, _port) = the_page_job(&mut browse);
    assert_eq!(job.req.server(), (sid, ClientKey::of(client)), "the read asks with the current key");
    assert!(browse.adapter.page.busy(), "and holds the claim");
}

/// A refused page job leaves the listing exactly as a page that never started leaves it: the next
/// step asks for the SAME first missing page, not the one after it.
#[test]
fn a_refused_page_is_asked_for_again_at_the_same_offset() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, sid, _client) = loopback_page_source();
    browse.maybe_spawn();
    let (job, port) = the_page_job(&mut browse);
    let start = job.req.start;
    port.refused(job.addr);

    browse.maybe_spawn();
    let (again, _port) = the_page_job(&mut browse);
    assert_eq!(again.req.start, start, "the same page is asked for again");
    assert_eq!(again.req.server().0, sid);
    assert_ne!(again.addr, job.addr, "…under a fresh request id");
}

/// A directory job whose key moved by the time the runner launched it is refused, for the genre
/// list and the letter counts alike: the next tick settles it (the claim is down, the list still
/// owed, the record consumed) and asks nothing while the registry holds no client. The tick after,
/// with the client back under a new key, asks again with THAT key.
#[test]
fn a_refused_directory_job_is_settled_by_the_next_step_and_the_one_after_asks_again_with_the_new_key() {
    for (which, work) in [(Directory::Genres, LibraryWork::Genres), (Directory::Letters, LibraryWork::Letters)] {
        let _g = plx_base::testlock::serial();
        let (_cleanup, mut browse, sid, client) = loopback_page_source();
        let first = ClientKey::of(client);
        tick_directory(&mut browse, sid, work.clone());
        let (job, port) = the_directory_job(&mut browse);
        assert_eq!(job.req.server(), (sid, first), "premise: the first read asked with the first key");
        assert!(which.busy(&browse.adapter), "premise: the step raised the claim");

        port.refused(job.addr); // the runner found the registry's client moved at the launch
        assert!(!which.busy(&browse.adapter), "the refusal gave the claim back in the call that refused");

        // The next tick finds no client: it settles the refusal and has nothing to ask with.
        let jobs = tick_directory_without_keys(&mut browse, sid, work.clone());
        assert!(jobs.is_empty(), "no client, no read: {jobs:?}");
        assert!(!which.busy(&browse.adapter), "no claim is left up without a job in flight");
        assert!(!directory_refusal_pending(&browse, which, job.addr.req.0), "the refusal is consumed");
        let state = &browse.state.states()[0];
        assert!(!state.genres_done && !state.letters_done, "the list is still owed, as after a read that never started");

        client.set_token("new");
        assert_ne!(ClientKey::of(client), first, "premise: the key moved");
        tick_directory(&mut browse, sid, work);
        let (job, _port) = the_directory_job(&mut browse);
        assert_eq!(job.req.server(), (sid, ClientKey::of(client)), "the read asks with the current key");
        assert!(which.busy(&browse.adapter), "and holds the claim");
    }
}

/// The two directory reads claim apart: a genre read out does not hold the letters' tick, and the
/// refusal of one settles only that one.
#[test]
fn a_refused_genre_read_leaves_the_letters_read_in_flight() {
    let _g = plx_base::testlock::serial();
    let (_cleanup, mut browse, sid, _client) = loopback_page_source();
    tick_directory(&mut browse, sid, LibraryWork::Genres);
    tick_directory(&mut browse, sid, LibraryWork::Letters);
    let jobs = std::mem::take(&mut browse.jobs);
    assert_eq!(jobs.len(), 2, "one read each: {jobs:?}");
    let StoreJob::BrowseDirectory(genres, port) = &jobs[0] else { panic!("genres first") };
    port.refused(genres.addr);

    assert!(!Directory::Genres.busy(&browse.adapter), "the refused read gave its claim back");
    assert!(Directory::Letters.busy(&browse.adapter), "the other read is still out");
}
