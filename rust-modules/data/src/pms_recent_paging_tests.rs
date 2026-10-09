use super::*;
use super::test_support::*;

fn owner() -> Owner {
    plx_plex::plex::reset_servers_for_test();
    let sid = plx_plex::plex::register_for_test("recent", "127.0.0.1", 9, "synthetic", "fixture");
    let mut owner = Owner::default();
    let mut shelf = page(0, 12, true).shelves.remove(0);
    shelf.title = "Recent".into();
    seed(&mut owner.state, vec![src(sid.raw(), "", HubState::Ready, Some(built(sid.raw(), &[], vec![shelf])))]);
    owner
}

fn page(start: usize, count: usize, more: bool) -> SourceBuild {
    let keys: Vec<_> = (start..start + count).map(|i| i.to_string()).collect();
    let mut shelf = shelf(0, "", "home.movies.recent", &keys.iter().map(String::as_str).collect::<Vec<_>>());
    shelf.key = "/hubs/home/recentlyAdded?type=1".into();
    shelf.offset = start;
    shelf.end = start + count;
    shelf.more = more;
    shelf.total = 12000;
    built(0, &[], vec![shelf])
}

fn ask(owner: &mut Owner, before: bool) -> Option<HubRequest> {
    let mut held = None;
    let _ = request_page(&mut owner.state, &owner.adapter, sid(0), "home.movies.recent",
        "/hubs/home/recentlyAdded?type=1", before, &mut |request| { held = Some(request); true });
    held
}

fn deliver(owner: &mut Owner, request: HubRequest, build: Option<SourceBuild>) {
    let result = request.complete(build);
    let encoded = record::encode(&result);
    let result = record::decode(encoded, |_| plx_plex::plex::client_for(sid(0))).unwrap();
    let _ = super::land(&mut owner.state, &owner.adapter, &result);
}

#[test]
fn recent_paging_crosses_many_windows_and_returns_without_growing_the_catalog() {
    let _guard = plx_base::testlock::serial();
    let mut owner = owner();
    let heroes: Vec<_> = (0..hubs_snapshot(&owner.state).view().hero_count())
        .map(|i| hubs_snapshot(&owner.state).view().hero(i).unwrap().item.rk.clone()).collect();
    for start in (0..1200).step_by(12) {
        let request = ask(&mut owner, false).unwrap();
        assert_eq!(request.page.as_ref().unwrap().start, start);
        assert!(ask(&mut owner, false).is_none(), "one page in flight per source");
        assert_eq!(hub_state(&owner.state), HubState::Ready);
        deliver(&mut owner, request, Some(page(start, 24, true)));
        let snapshot = hubs_snapshot(&owner.state);
        let view = snapshot.view();
        let hub = view.hub(0).unwrap();
        assert_eq!(hub.offset, start);
        assert_eq!(hub.items.len(), 24);
        assert_eq!(hub.items[0].rk, start.to_string());
        assert!(snapshot.data.items.len() <= 24 + HERO_MAX);
        let current: Vec<_> = (0..view.hero_count()).map(|i| view.hero(i).unwrap().item.rk.clone()).collect();
        assert_eq!(current, heroes);
    }
    let request = ask(&mut owner, true).unwrap();
    assert_eq!(request.page.as_ref().unwrap().start, 1176);
    deliver(&mut owner, request, Some(page(1176, 24, true)));
    assert_eq!(rks(&owner.state, 0)[0], "1176");
    reset(&mut owner.state, &owner.adapter);
    plx_plex::plex::reset_servers_for_test();
}

#[test]
fn failed_pages_retry_without_blanking_home_and_old_profile_results_are_rejected() {
    let _guard = plx_base::testlock::serial();
    let mut owner = owner();
    let before = rks(&owner.state, 0);
    let request = ask(&mut owner, false).unwrap();
    deliver(&mut owner, request, None);
    assert_eq!(rks(&owner.state, 0), before);
    assert_eq!(hub_state(&owner.state), HubState::Ready);
    let mut held = None;
    let _ = step_landings_with(&mut owner.state, &owner.adapter, Some(RETRY_MIN_S), Vec::new,
        &mut |request| { held = Some(request); true });
    let request = held.unwrap();
    assert_eq!(request.page.as_ref().unwrap().start, 0);
    plx_plex::plex::client_for(sid(0)).unwrap().set_token("changed-profile");
    deliver(&mut owner, request, Some(page(0, 24, true)));
    assert_eq!(rks(&owner.state, 0), before);
    reset(&mut owner.state, &owner.adapter);
    plx_plex::plex::reset_servers_for_test();
}

#[test]
fn a_short_or_empty_last_page_stops_further_requests() {
    let _guard = plx_base::testlock::serial();
    for count in [0, 18] {
        let mut owner = owner();
        let request = ask(&mut owner, false).unwrap();
        deliver(&mut owner, request, Some(page(0, count, false)));
        assert_eq!(hub_len(&owner.state, 0), if count == 0 { 12 } else { count });
        assert!(ask(&mut owner, false).is_none());
        assert!(ask(&mut owner, true).is_none());
        reset(&mut owner.state, &owner.adapter);
    }
    plx_plex::plex::reset_servers_for_test();
}

#[test]
fn a_page_reads_the_provider_listing_and_counts_raw_results_before_filtering() {
    use std::io::{Read, Write};
    let _guard = plx_base::testlock::serial();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        let (mut socket, _) = loop {
            match plx_base::testnet::accept(&listener) {
                Ok(socket) => break socket,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock && std::time::Instant::now() < deadline =>
                    std::thread::sleep(std::time::Duration::from_millis(5)),
                Err(error) => panic!("page fixture accept failed: {error}"),
            }
        };
        socket.set_read_timeout(Some(std::time::Duration::from_secs(3))).unwrap();
        let mut request = [0; 4096];
        let n = socket.read(&mut request).unwrap();
        let request = String::from_utf8_lossy(&request[..n]);
        assert!(request.starts_with("GET /hubs/home/recentlyAdded?type=1&sectionID=7&X-Plex-Container-Start=12&X-Plex-Container-Size=24"));
        let body = r#"{"MediaContainer":{"offset":12,"totalSize":14,"Metadata":[
          {"ratingKey":"12","type":"movie","title":"Movie","thumb":"/poster","librarySectionID":7},
          {"ratingKey":"13","type":"movie","title":"No poster","librarySectionID":7}]}}"#;
        write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
    });
    plx_plex::plex::reset_servers_for_test();
    let registered = plx_plex::plex::register_for_test("page-http", "127.0.0.1", i32::from(port), "synthetic", "fixture");
    let client = plx_plex::plex::client_for(registered).unwrap();
    let page = fetch_page(client, registered, &PageQuery { id: "home.movies.recent".into(),
        key: "/hubs/home/recentlyAdded?type=1&sectionID=7".into(), start: 12 }).unwrap();
    server.join().unwrap();
    assert_eq!(page.shelves[0].items.len(), 1);
    assert_eq!(page.shelves[0].end, 14);
    assert!(!page.shelves[0].more);
    assert_eq!(page.shelves[0].items[0].sid, sid(0));
    assert_eq!(page.shelves[0].items[0].sec, 7);
    plx_plex::plex::reset_servers_for_test();
}

#[test]
fn a_normal_refetch_reloads_the_current_recent_window() {
    check_refetch_window(false);
}

#[test]
fn a_profile_refetch_starts_at_the_preview_instead_of_the_old_window() {
    check_refetch_window(true);
}

fn check_refetch_window(change_profile: bool) {
    use std::io::{Read, Write};
    let _guard = plx_base::testlock::serial();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        let mut paths = Vec::new();
        while std::time::Instant::now() < deadline && paths.len() < 3 {
            let (mut socket, _) = match plx_base::testnet::accept(&listener) {
                Ok(socket) => socket,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                    continue;
                }
                Err(error) => panic!("refresh fixture accept failed: {error}"),
            };
            socket.set_read_timeout(Some(std::time::Duration::from_secs(3))).unwrap();
            let mut request = [0; 4096];
            let n = socket.read(&mut request).unwrap();
            let path = String::from_utf8_lossy(&request[..n]).split_whitespace().nth(1).unwrap().to_owned();
            let metadata = |start: usize, count: usize| (start..start + count).map(|i| serde_json::json!({
                "ratingKey":i.to_string(),"type":"movie","title":"Refreshed movie","thumb":"/poster"
            })).collect::<Vec<_>>();
            let body = if path.starts_with("/hubs/home/recentlyAdded?") {
                serde_json::json!({"MediaContainer":{"offset":36,"totalSize":12000,"Metadata":metadata(36,24)}})
            } else if path.starts_with("/hubs/continueWatching?") {
                serde_json::json!({"MediaContainer":{"Hub":[]}})
            } else {
                serde_json::json!({"MediaContainer":{"Hub":[{"hubIdentifier":"home.movies.recent",
                    "type":"movie","title":"Recent","key":"/hubs/home/recentlyAdded?type=1",
                    "more":true,"Metadata":metadata(0,12)}]}})
            }.to_string();
            write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
            paths.push(path);
        }
        paths
    });
    plx_plex::plex::reset_servers_for_test();
    let registered = plx_plex::plex::register_for_test("refresh-http", "127.0.0.1", i32::from(port), "synthetic", "fixture");
    let mut owner = Owner::default();
    seed(&mut owner.state, vec![src(registered.raw(), "", HubState::Ready, Some(page(36,24,true)))]);
    if change_profile { plx_plex::plex::client_for(registered).unwrap().set_token("new-synthetic-profile"); }
    let mut held = None;
    let _ = request_refetch_hubs_with_scope(&mut owner.state, &owner.adapter, &BrowseScope::standalone(),
        &mut |request| { held = Some(request); true });
    let request = held.unwrap();
    let result = request.fetch();
    deliver(&mut owner, request, result);
    let paths = server.join().unwrap();
    assert_eq!(hubs_snapshot(&owner.state).view().hub(0).unwrap().offset,if change_profile { 0 } else { 36 });
    assert_eq!(rks(&owner.state,0).contains(&"54".into()),!change_profile);
    assert_eq!(paths.iter().any(|path| path.contains("X-Plex-Container-Start=36")),!change_profile);
    assert_eq!(hubs_snapshot(&owner.state).view().hub(0).unwrap().items[0].title,"Refreshed movie");
    reset(&mut owner.state,&owner.adapter);
    plx_plex::plex::reset_servers_for_test();
}
