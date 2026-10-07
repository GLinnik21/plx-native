//! Dump mode at the Browse sites (discovery, genre and letters directories, listing page, section
//! hubs): each takes through its OWN mailbox's claim (`src_fetching`, `genre_fetching`,
//! `letters_fetching`, `fetching`, `hubs.fetching`), with a dump-armed local gate and a worker that
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
    browse.adapter.src_fetching.store(true, Ordering::SeqCst); // the discovery request is out
    let worker = Arc::clone(&browse.adapter);
    let join = late(move || {
        *worker.src_result.lock().unwrap() = Some((epoch, 0, SrcLanding {
            client, token_gen, name: "late-name".into(),
            what: SrcWhat::Sections(Some(vec![(99, "Late Library".into(), SecKind::Movie)])),
        }));
    });
    let gate = dump_gate();
    let outcome = browse.state.land_discovery_owned_with_gate(&browse.adapter, &gate);
    assert!(outcome.changed, "the answer the pump owed must be taken by the pump that asked");
    assert!(!browse.adapter.src_fetching.load(Ordering::SeqCst), "the take released the claim");
    join.join().unwrap();
}

#[test]
fn dump_mode_a_directory_request_out_lands_on_the_pump_that_runs_whatever_the_worker() {
    let _serial = plx_base::testlock::serial();
    for letters in [false, true] {
        let (_cleanup, mut browse, _sid, client) = registered_directory_source();
        let (epoch, token_gen) = (browse.state.table_epoch(), client.token_gen());
        let library_type = browse.state.states[0].library_type;
        let adapter = Arc::clone(&browse.adapter);
        let (flag, mail_genre, mail_letters) =
            (if letters { &adapter.letters_fetching } else { &adapter.genre_fetching },
             &adapter.genre_result, &adapter.letter_result);
        flag.store(true, Ordering::SeqCst);
        let worker = Arc::clone(&adapter);
        let join = late(move || {
            if letters {
                *worker.letter_result.lock().unwrap() = Some(DirectoryResult {
                    library_type, epoch, sec: 0, client, token_gen, list: vec![("S".into(), 99)],
                });
            } else {
                *worker.genre_result.lock().unwrap() = Some(DirectoryResult {
                    library_type, epoch, sec: 0, client, token_gen,
                    list: vec![GenreEntry { id: "late".into(), title: "Late".into() }],
                });
            }
        });
        let gate = dump_gate();
        let changed = if letters {
            browse.state.land_directory_owned_with_gate(&gate, flag, mail_letters, |st, list| {
                st.letters_done = true;
                st.letters = Arc::new(list);
            })
        } else {
            browse.state.land_directory_owned_with_gate(&gate, flag, mail_genre, |st, list| {
                st.genres_done = true;
                st.genres = Arc::new(list);
            })
        };
        assert!(changed, "the answer the pump owed must be taken by the pump that asked (letters={letters})");
        assert!(!flag.load(Ordering::SeqCst), "the take released the claim (letters={letters})");
        join.join().unwrap();
    }
}

#[test]
fn dump_mode_a_page_request_out_lands_on_the_pump_that_runs_whatever_the_worker() {
    let _serial = plx_base::testlock::serial();
    let (_cleanup, mut browse, sid, client) = registered_page_source();
    let (gen, token_gen) = (browse.state.query_gen(), client.token_gen());
    browse.adapter.fetching.store(true, Ordering::SeqCst); // the page request is out
    let worker = Arc::clone(&browse.adapter);
    let join = late(move || {
        *worker.page_result.lock().unwrap() = Some(PageResult {
            client, token_gen, gen, sec: 0, start: 0,
            items: vec![PmsMovie { sid, ..Default::default() }], total: 1,
            sorts: None, restored: None, genres: None, genre: None, resolved: Default::default(),
        });
    });
    let gate = dump_gate();
    let outcome = browse.state.pump_owned_with_gate(&browse.adapter, &gate);
    assert!(outcome.changed, "the answer the pump owed must be taken by the pump that asked");
    assert!(!browse.adapter.fetching.load(Ordering::SeqCst), "the take released the claim");
    assert_eq!(browse.state.cur_state().map(|st| st.total), Some(1));
    join.join().unwrap();
}

#[test]
fn dump_mode_a_hubs_request_out_lands_on_the_pump_that_runs_whatever_the_worker() {
    let _serial = plx_base::testlock::serial();
    let (_cleanup, mut browse, _sid, client) = registered_page_source();
    let (epoch, token_gen) = (browse.state.table_epoch(), client.token_gen());
    browse.adapter.hubs.fetching.store(true, Ordering::SeqCst); // the section-hubs request is out
    let worker = Arc::clone(&browse.adapter);
    let join = late(move || {
        *worker.hubs.result.lock().unwrap() =
            Some(section_hubs::HubResult::failed_for_test(epoch, 0, client, token_gen));
    });
    let gate = dump_gate();
    browse.state.hubs_land(&browse.adapter, &gate);
    assert!(browse.adapter.hubs.result.lock().unwrap().is_none(), "the pump that asked took the answer");
    assert!(!browse.adapter.hubs.fetching.load(Ordering::SeqCst), "the take released the claim");
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
    browse.adapter.genre_fetching.store(true, Ordering::SeqCst);
    browse.state.epoch = browse.state.epoch.wrapping_add(1); // the table moved while the worker was out
    let worker = Arc::clone(&browse.adapter);
    let join = late(move || {
        *worker.genre_result.lock().unwrap() = Some(DirectoryResult {
            library_type, epoch: stale_epoch, sec: 0, client, token_gen,
            list: vec![GenreEntry { id: "stale".into(), title: "Stale".into() }],
        });
    });
    let gate = dump_gate();
    let adapter = Arc::clone(&browse.adapter);
    let changed = browse.state.land_directory_owned_with_gate(
        &gate, &adapter.genre_fetching, &adapter.genre_result, |st, list| {
        st.genres_done = true;
        st.genres = Arc::new(list);
    });
    assert!(!changed, "a stale answer changes nothing");
    assert!(!adapter.genre_fetching.load(Ordering::SeqCst), "but it was awaited, and its take released the claim");
    assert!(!browse.state.states[0].genres_done);
    join.join().unwrap();
}
