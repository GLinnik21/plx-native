//! A season switch and an episode-page window are typed jobs the owner emits, not threads the data
//! layer starts (`docs/sans-io.md`): the step leaves its request on the outbox and starts nothing.

use super::*;
use super::test_support::episode;
use crate::stores::metadata::{MetadataCmd, MetadataStore};
use crate::stores::StoreJob;
use plx_machine::job::{Job, Port};
use plx_plex::plex::ServerId;

fn show_on(store: &mut MetadataStore, sid: plx_plex::plex::ServerId) {
    let season = |index: i64| Season {
        rk: format!("sk{index}"), index, title: format!("Season {index}"), leaf_count: 0, viewed_leaf_count: 0,
    };
    let mut episodes = EpisodeList::with_total(200);
    episodes.set_page(0, (0..EPISODE_PAGE).map(|i| episode(&format!("e{i}"))).collect());
    set_current_for_test(store.state_mut(), Some(Detail {
        sid, rk: "show".into(), is_show: true, seasons: vec![season(1), season(2)], cur_season: 0,
        episodes, ..Default::default()
    }));
}

/// The sid of a server registered at a port nothing listens on: a worker that did start would
/// answer at once, with a failure, so "nothing answered" proves nothing started.
fn dead_server() -> ServerId {
    plx_plex::plex::reset_servers_for_test();
    plx_plex::plex::register_for_test("season-job", "127.0.0.1", 9, "t", "cid-season-job")
}

fn season_job(store: &mut MetadataStore) -> (plx_machine::machine::Addr, MetaSeasonReq, MetaSeasonPort) {
    let mut jobs = store.take_jobs();
    assert_eq!(jobs.len(), 1, "one switch, one request");
    match jobs.remove(0) {
        StoreJob::MetaSeason(Job { addr, req }, port) => (addr, req, port),
        other => panic!("not a season job: {other:?}"),
    }
}

fn pages_job(store: &mut MetadataStore) -> (plx_machine::machine::Addr, MetaEpisodesReq, MetaEpisodesPort) {
    let mut jobs = store.take_jobs();
    assert_eq!(jobs.len(), 1, "one window, one request");
    match jobs.remove(0) {
        StoreJob::MetaEpisodes(Job { addr, req }, port) => (addr, req, port),
        other => panic!("not a page job: {other:?}"),
    }
}

fn want(store: &mut MetadataStore) -> bool {
    store.run(MetadataCmd::WantEpisodes { lo: EPISODE_PAGE, hi: EPISODE_PAGE + 10, focus: None, restore: None })
}

/// A season switch leaves one request on the outbox and starts no worker of its own: the job names
/// the show's server, the season, the tab it came from and its generation (its address), and the
/// registered server it names is never dialled.
#[test]
fn a_season_switch_emits_its_request_and_starts_no_worker() {
    let _serial = plx_base::testlock::serial();
    let sid = dead_server();
    let mut store = MetadataStore::default();
    show_on(&mut store, sid);
    assert!(!store.has_jobs());

    store.run(MetadataCmd::LoadSeason(1));
    plx_base::task::drain_workers_for_test();

    let adapter = store.adapter_for_test();
    let gen = adapter.season_gen.load(std::sync::atomic::Ordering::SeqCst);
    let stamp = store.state().ledger.stamp();
    plx_machine::assert_fx!(store.take_jobs(), [StoreJob::MetaSeason(
        Job {
            addr: season_addr(gen),
            req: MetaSeasonReq { sid, show: "show".into(), season: "sk2".into(), idx: 1, prev: 0, stamp },
        },
        MetaSeasonPort::of(&adapter),
    )]);
    assert!(adapter.season_result.lock().unwrap().is_none(), "no worker ran: nothing was answered");
    assert!(season_loading(&adapter), "the request is out");
    assert_eq!(store.state().current.as_ref().map(|d| d.cur_season), Some(1), "the tab flipped at once");
}

/// An episode-page window leaves one request on the outbox and starts no worker of its own.
#[test]
fn an_episode_window_emits_its_request_and_starts_no_worker() {
    let _serial = plx_base::testlock::serial();
    let sid = dead_server();
    let mut store = MetadataStore::default();
    show_on(&mut store, sid);
    assert!(!store.has_jobs());

    assert!(want(&mut store), "the window had a hole to fetch");
    plx_base::task::drain_workers_for_test();

    let adapter = store.adapter_for_test();
    let stamp = store.state().ledger.stamp();
    let key = (sid, "sk1".to_string(), 1);
    plx_machine::assert_fx!(store.take_jobs(), [StoreJob::MetaEpisodes(
        Job {
            addr: pages_addr(1),
            req: MetaEpisodesReq { sid, show: "show".into(), season: "sk1".into(), pages: vec![1], stamp },
        },
        MetaEpisodesPort::new(&adapter, vec![key.clone()]),
    )]);
    let ep = adapter.ep_pages.lock().unwrap();
    assert!(ep.landed.is_empty(), "no worker ran: nothing was answered");
    assert_eq!(ep.inflight, vec![key], "the job holds its page");
}

/// A season job nobody started: the refusal catches DONE up, so the episode row does not keep its
/// spinner and the play gate does not stay shut. The tab stays where the user put it and the
/// old episodes stay listed, exactly what the inline spawn's `!spawned` arm left.
#[test]
fn a_refused_season_job_ends_season_loading_and_leaves_the_tab_and_the_list() {
    let _serial = plx_base::testlock::serial();
    let mut store = MetadataStore::default();
    show_on(&mut store, dead_server());
    store.run(MetadataCmd::LoadSeason(1));
    let adapter = store.adapter_for_test();
    assert!(season_loading(&adapter));
    let (addr, _, port) = season_job(&mut store);

    port.refused(addr);

    assert!(!season_loading(&adapter), "refused: nothing is owed");
    assert!(!store.pump_season_with_gate(plx_machine::landgate::fixture_gate()), "and nothing landed");
    let d = store.state().current.as_ref().unwrap();
    assert_eq!(d.cur_season, 1, "the tab stays on the flipped season");
    assert_eq!(d.episodes.len(), 200, "the old list is still listed");
}

/// A refusal that arrives after a NEWER request has landed must not turn the spinner back on: DONE
/// only moves up.
#[test]
fn a_stale_season_refusal_does_not_rearm_season_loading() {
    let _serial = plx_base::testlock::serial();
    let sid = dead_server();
    let mut store = MetadataStore::default();
    show_on(&mut store, sid);
    store.run(MetadataCmd::LoadSeason(1));
    let (old_addr, _, old_port) = season_job(&mut store);
    store.run(MetadataCmd::LoadSeason(0));
    let (new_addr, new_req, new_port) = season_job(&mut store);
    let adapter = store.adapter_for_test();

    finish_season_with(&new_port, new_addr, plx_plex::plex::client_for(sid), new_req, |_, _| {
        let mut eps = EpisodeList::with_total(1);
        eps.set_page(0, vec![episode("fresh")]);
        Some(eps)
    });
    assert!(store.pump_season_with_gate(plx_machine::landgate::fixture_gate()), "the newer switch landed");
    assert!(!season_loading(&adapter));

    old_port.refused(old_addr);

    assert!(!season_loading(&adapter), "a stale refusal settles nothing new");
}

/// The server has no client at the launch (the registry moved on): the body answers the FAILURE
/// without a GET, which is today's failure path: the tab goes back and the list stays.
#[test]
fn a_season_job_whose_server_has_no_client_answers_the_failure() {
    let _serial = plx_base::testlock::serial();
    let mut store = MetadataStore::default();
    show_on(&mut store, dead_server());
    store.run(MetadataCmd::LoadSeason(1));
    let adapter = store.adapter_for_test();
    let (addr, req, port) = season_job(&mut store);

    finish_season_job(&port, addr, None, req);

    assert!(!store.pump_season_with_gate(plx_machine::landgate::fixture_gate()), "a failure changes no list");
    assert!(!season_loading(&adapter), "and it settled the request");
    assert_eq!(store.state().current.as_ref().unwrap().cur_season, 0, "the tab went back");
}

/// A fetch that panics lands as a FAILURE, not as an empty season and not as nothing: the answer is
/// posted outside the guard, so `season_loading()` does not report an in-flight fetch forever.
#[test]
fn a_panicking_season_fetch_lands_as_a_failure() {
    let _serial = plx_base::testlock::serial();
    let sid = dead_server();
    let mut store = MetadataStore::default();
    show_on(&mut store, sid);
    store.run(MetadataCmd::LoadSeason(1));
    let adapter = store.adapter_for_test();
    let (addr, req, port) = season_job(&mut store);

    finish_season_with(&port, addr, plx_plex::plex::client_for(sid), req, |_, _| panic!("a broken parser"));

    assert!(!store.pump_season_with_gate(plx_machine::landgate::fixture_gate()));
    assert!(!season_loading(&adapter), "the failure settled the request");
    assert_eq!(store.state().current.as_ref().unwrap().cur_season, 0, "the tab went back");
}

/// Page jobs nobody started: the refusal drops the inflight keys the job held, so the next
/// `WantEpisodes` asks for the same page again (while the job was out it would not).
#[test]
fn a_refused_page_job_releases_its_inflight_keys_and_the_next_want_asks_again() {
    let _serial = plx_base::testlock::serial();
    let mut store = MetadataStore::default();
    show_on(&mut store, dead_server());
    assert!(want(&mut store));
    let adapter = store.adapter_for_test();
    let (addr, _, port) = pages_job(&mut store);
    assert!(!want(&mut store), "the page is in flight: it is not asked for twice");
    assert!(!store.has_jobs());

    port.refused(addr);

    assert!(adapter.ep_pages.lock().unwrap().inflight.is_empty(), "the refusal released the key");
    assert!(want(&mut store), "the next window asks for the page again");
    assert_eq!(store.take_jobs().len(), 1);
}

/// The server has no client at the launch: every page of the job answers the failure without a GET
/// and releases its key, so the next window retries.
#[test]
fn a_page_job_whose_server_has_no_client_answers_each_page_as_a_failure() {
    let _serial = plx_base::testlock::serial();
    let mut store = MetadataStore::default();
    show_on(&mut store, dead_server());
    assert!(want(&mut store));
    let adapter = store.adapter_for_test();
    let (addr, req, port) = pages_job(&mut store);

    finish_episodes_job(&port, addr, None, req);

    let ep = adapter.ep_pages.lock().unwrap();
    assert!(ep.inflight.is_empty(), "answered: the key came back");
    assert_eq!(ep.landed.len(), 1);
    assert!(ep.landed[0].rows.is_none(), "a failure, not an empty page");
}

/// A read that panics lands that page as a failure and the job still answers, outside the guard.
#[test]
fn a_panicking_page_read_lands_as_a_failure() {
    let _serial = plx_base::testlock::serial();
    let sid = dead_server();
    let mut store = MetadataStore::default();
    show_on(&mut store, sid);
    assert!(want(&mut store));
    let adapter = store.adapter_for_test();
    let (addr, req, port) = pages_job(&mut store);

    finish_episodes_with(&port, addr, plx_plex::plex::client_for(sid), req, |_, _, _| panic!("a broken parser"));

    let ep = adapter.ep_pages.lock().unwrap();
    assert!(ep.inflight.is_empty());
    assert_eq!(ep.landed.len(), 1);
    assert!(ep.landed[0].rows.is_none());
}

/// A page job lands each page as it is read, not all of them after the last read. The window of a
/// restore far down a long season asks for the restore page first and two more behind it, so the
/// page the strip is waiting on must seat after ITS read, not after three serial ones (on a relayed
/// server that is two round trips of a visible hole).
#[test]
fn a_page_job_lands_each_page_as_it_is_read() {
    let _serial = plx_base::testlock::serial();
    let sid = dead_server();
    let mut store = MetadataStore::default();
    show_on(&mut store, sid);
    assert!(store.run(MetadataCmd::WantEpisodes {
        lo: EPISODE_PAGE, hi: 4 * EPISODE_PAGE, focus: None, restore: Some(2 * EPISODE_PAGE),
    }));
    let adapter = store.adapter_for_test();
    let (addr, req, port) = pages_job(&mut store);
    assert_eq!(req.pages, [2, 1, 3], "the restore page first, then the holes in order");

    let seen = std::sync::Mutex::new(Vec::new());
    let landed_before_each_read = |_: &'static plx_plex::plex::Client, _: &str, page: usize| {
        let ep = adapter.ep_pages.lock().unwrap();
        seen.lock().unwrap().push((page, ep.landed.iter().map(|l| l.page).collect::<Vec<_>>()));
        Some(vec![episode(&format!("e-page-{page}"))])
    };
    finish_episodes_with(&port, addr, plx_plex::plex::client_for(sid), req, landed_before_each_read);

    assert_eq!(
        *seen.lock().unwrap(),
        [(2, vec![]), (1, vec![2]), (3, vec![2, 1])],
        "each read found the earlier pages already landed"
    );
    let ep = adapter.ep_pages.lock().unwrap();
    assert!(ep.inflight.is_empty(), "every page gave its key back");
    assert_eq!(ep.landed.iter().map(|l| l.page).collect::<Vec<_>>(), [2, 1, 3]);
}
