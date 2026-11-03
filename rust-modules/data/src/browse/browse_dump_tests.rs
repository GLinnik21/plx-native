//! Dump mode at the Browse sites (discovery, genre and letters directories, listing page, section
//! hubs): each takes through its OWN mailbox's claim (`src_fetching`, `genres`, `letters`, `page`,
//! `hubs.fetch`), with a dump-armed local gate and a worker that
//! posts late. Each test fails if its site's take reverts to a plain one: the first poll finds an
//! empty mailbox, the pump moves on, and the claim is still held. See `plx_machine::landgate`'s
//! module doc for which sites dump mode covers.

use super::*;
use super::test_support::*;

const WORKER_DELAY: std::time::Duration = std::time::Duration::from_millis(25);

fn dump_gate() -> plx_machine::landgate::Gate {
    let gate = plx_machine::landgate::Gate::default();
    gate.arm_dump(std::time::Duration::from_secs(20));
    gate
}

fn late(post: impl FnOnce() + Send + 'static) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        std::thread::sleep(WORKER_DELAY);
        post();
    })
}

#[test]
fn dump_mode_a_discovery_request_out_lands_on_the_pump_that_runs_whatever_the_worker() {
    let _serial = plx_base::testlock::serial();
    let (_cleanup, mut browse, _sid, client) = registered_page_source();
    let (epoch, token_gen) = (browse.state.table_epoch(), client.token_gen());
    browse.adapter.src_fetching.claim(0); // the discovery request is out
    let worker = Arc::clone(&browse.adapter);
    let join = late(move || {
        *worker.src_result.lock().unwrap() = Some((epoch, 0, SrcLanding {
            client, token_gen, name: "late-name".into(),
            what: SrcWhat::Sections(Some(vec![(99, "Late Library".into(), SecKind::Movie)])),
        }));
    });
    let gate = dump_gate();
    let outcome = browse.state.land_discovery_owned_with_gate(&browse.adapter, &gate,
        &plx_plex::plex::session::peek());
    assert!(outcome.changed, "the answer the pump owed must be taken by the pump that asked");
    assert!(!browse.adapter.src_fetching.busy(), "the apply released the claim");
    join.join().unwrap();
}

#[test]
fn dump_mode_a_directory_request_out_lands_on_the_pump_that_runs_whatever_the_worker() {
    let _serial = plx_base::testlock::serial();
    for letters in [false, true] {
        let (_cleanup, mut browse, _sid, client) = registered_directory_source();
        let (epoch, token_gen) = (browse.state.table_epoch(), client.token_gen());
        let library_type = browse.state.states[0].library_type;
        let asked = browse.state.states[0].letters_key();
        let adapter = Arc::clone(&browse.adapter);
        if letters { adapter.letters.claim(0) } else { adapter.genres.claim(0) }
        let worker = Arc::clone(&adapter);
        let join = late(move || {
            if letters {
                worker.letters.post(DirectoryResult {
                    library_type, epoch, sec: 0, client, token_gen, list: vec![("S".into(), 99)],
                    ok: true, letters: Some(asked),
                }, |_| true);
            } else {
                worker.genres.post(DirectoryResult {
                    library_type, epoch, sec: 0, client, token_gen,
                    list: vec![GenreEntry { id: "late".into(), title: "Late".into() }],
                    ok: true, letters: None,
                }, |_| true);
            }
        });
        let gate = dump_gate();
        let changed = if letters {
            browse.state.land_letters_owned(&gate, &adapter.letters)
        } else {
            browse.state.land_directory_owned_with_gate(&gate, &adapter.genres, |st, list, _, _| {
                st.genres_done = true;
                st.genres = Arc::new(list);
            })
        };
        assert!(changed, "the answer the pump owed must be taken by the pump that asked (letters={letters})");
        assert!(!adapter.letters.busy() && !adapter.genres.busy(), "the take released the claim (letters={letters})");
        join.join().unwrap();
    }
}

#[test]
fn dump_mode_a_page_request_out_lands_on_the_pump_that_runs_whatever_the_worker() {
    let _serial = plx_base::testlock::serial();
    let (_cleanup, mut browse, sid, client) = registered_page_source();
    let (gen, token_gen) = (browse.state.query_gen(), client.token_gen());
    browse.adapter.page.claim(0); // the page request is out
    let worker = Arc::clone(&browse.adapter);
    let join = late(move || {
        worker.page.post(PageResult {
            client, token_gen, gen, sec: 0, start: 0,
            items: vec![PmsMovie { sid, ..Default::default() }], total: 1,
            sorts: None, restored: None, genres: None, genre: None, resolved: Default::default(),
        }, |_| true);
    });
    let gate = dump_gate();
    let outcome = browse.state.pump_owned_with_gate(&browse.adapter, &gate, &ClientKeys::default(),
        &plx_plex::plex::session::peek(), &mut Vec::new());
    assert!(outcome.changed, "the answer the pump owed must be taken by the pump that asked");
    assert!(!browse.adapter.page.busy(), "the take released the claim");
    assert_eq!(browse.state.cur_state().map(|st| st.total), Some(1));
    join.join().unwrap();
}

#[test]
fn dump_mode_a_hubs_request_out_lands_on_the_pump_that_runs_whatever_the_worker() {
    let _serial = plx_base::testlock::serial();
    let (_cleanup, mut browse, _sid, _client) = registered_page_source();
    browse.hubs_kick(0); // the section-hubs request is out
    assert!(browse.adapter.hubs.fetch.busy(), "premise: the request holds the claim");
    let (job, port) = browse.take_hubs_job();
    let join = late(move || {
        plx_machine::job::Port::done(&port, job.addr, section_hubs::BrowseHubsAnswer::failed_for_test(&job));
    });
    let gate = dump_gate();
    browse.hubs_land_on(&gate);
    assert!(!browse.adapter.hubs.fetch.has_mail(), "the pump that asked took the answer");
    assert!(!browse.adapter.hubs.fetch.busy(), "the take released the claim");
    join.join().unwrap();
}

/// A stale answer (an epoch bump while the worker was out, with no adapter rotation) is awaited
/// first and consumed by the pump that was owed it: the claim is released by THAT take, so no new
/// request is ever out behind it, and no later stale arrival can release a newer claim. (An epoch
/// bump that rotates the adapter, as a roster retire does, retires the old claim with the old
/// adapter.)
#[test]
fn dump_mode_a_stale_epoch_answer_is_awaited_and_consumed_by_the_pump_that_was_owed_it() {
    let _serial = plx_base::testlock::serial();
    let (_cleanup, mut browse, _sid, client) = registered_directory_source();
    let stale_epoch = browse.state.table_epoch();
    let token_gen = client.token_gen();
    let library_type = browse.state.states[0].library_type;
    browse.adapter.genres.claim(0);
    browse.state.epoch = browse.state.epoch.wrapping_add(1); // the table moved while the worker was out
    let worker = Arc::clone(&browse.adapter);
    let join = late(move || {
        worker.genres.post(DirectoryResult {
            library_type, epoch: stale_epoch, sec: 0, client, token_gen,
            list: vec![GenreEntry { id: "stale".into(), title: "Stale".into() }],
            ok: true, letters: None,
        }, |_| true);
    });
    let gate = dump_gate();
    let adapter = Arc::clone(&browse.adapter);
    let changed = browse.state.land_directory_owned_with_gate(&gate, &adapter.genres, |st, list, _, _| {
        st.genres_done = true;
        st.genres = Arc::new(list);
    });
    assert!(!changed, "a stale answer changes nothing");
    assert!(!adapter.genres.busy(), "but it was awaited, and its take released the claim");
    assert!(!browse.state.states[0].genres_done);
    join.join().unwrap();
}

/// The paced dump keeps one claim per CALL SITE (`Gate::take_owed` is `#[track_caller]`), so a
/// wrapper between a take and its callers must be `#[track_caller]` too, or every mailbox behind it
/// shares one key. `land_directory_owned_with_gate` serves two mailboxes, genre and letters (the letters through `land_letters_owned`, which hands its caller's site on as well): with a
/// lag, each must be held until `lag` frames after ITS OWN claim was first seen open. Genre is seen
/// at frame 0 and letters at frame 2 (lag 3), so genre lands on frame 3 and letters on frame 5; under
/// a shared key letters inherits genre's sighting and lands on a frame that depends on which of the
/// two took first and whether the other had landed.
#[test]
fn a_paced_lag_counts_the_genre_and_letters_mailboxes_from_their_own_first_sighting() {
    let _serial = plx_base::testlock::serial();
    let (_cleanup, mut browse, _sid, client) = registered_directory_source();
    let (epoch, token_gen) = (browse.state.table_epoch(), client.token_gen());
    let library_type = browse.state.states[0].library_type;
    let asked = browse.state.states[0].letters_key();
    let adapter = Arc::clone(&browse.adapter);
    let gate = plx_machine::landgate::Gate::default();
    gate.arm_dump_paced(3);
    adapter.genres.post(DirectoryResult {
        library_type, epoch, sec: 0, client, token_gen,
        list: vec![GenreEntry { id: "g".into(), title: "G".into() }],
        ok: true, letters: None,
    }, |_| true);
    adapter.genres.claim(0);
    let (mut genre_at, mut letters_at) = (None, None);
    for v in 0..9u64 {
        gate.dump_frame(v);
        if v == 2 {
            adapter.letters.post(DirectoryResult {
                library_type, epoch, sec: 0, client, token_gen, list: vec![("S".into(), 9)],
                ok: true, letters: Some(asked.clone()),
            }, |_| true);
            adapter.letters.claim(0);
        }
        if genre_at.is_none() && browse.state.land_directory_owned_with_gate(
            &gate, &adapter.genres, |st, list, _, _| {
            st.genres_done = true;
            st.genres = Arc::new(list);
        }) {
            genre_at = Some(v);
        }
        if letters_at.is_none() && browse.state.land_letters_owned(&gate, &adapter.letters) {
            letters_at = Some(v);
        }
    }
    assert_eq!((genre_at, letters_at), (Some(3), Some(5)),
        "each mailbox lands a lag after its own first sighting (genre, letters)");
}
