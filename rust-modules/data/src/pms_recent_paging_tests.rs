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
    shelf.positions = (start..start + count).collect();
    shelf.more = more;
    shelf.total = 12000;
    built(0, &[], vec![shelf])
}

fn ask(owner: &mut Owner, before: bool) -> Option<HubRequest> {
    let mut held = None;
    let _ = request_page(&mut owner.state, &owner.adapter, sid(0), "home.movies.recent",
        "/hubs/home/recentlyAdded?type=1", before, &BrowseScope::standalone(), &mut |request| { held = Some(request); true });
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
        assert_eq!(request.page.as_ref().unwrap().start, start + 12);
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
    assert_eq!(request.page.as_ref().unwrap().start, 1188);
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
    assert_eq!(request.page.as_ref().unwrap().start, 12);
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
        key: "/hubs/home/recentlyAdded?type=1&sectionID=7".into(), start: 12, before: false, hidden: Vec::new(), reload: false }, 0).unwrap();
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

#[test]
fn a_page_that_never_succeeds_stops_retrying_and_releases_the_source() {
    let _guard = plx_base::testlock::serial();
    let mut owner = owner();
    let cards = rks(&owner.state, 0);
    let mut request = ask(&mut owner, false).unwrap();
    for attempt in 0..3 {
        deliver(&mut owner, request, None);
        let mut retry = None;
        let _ = step_landings_with(&mut owner.state, &owner.adapter, Some(plx_plex::plex::retry::RETRY_MAX_S), Vec::new,
            &mut |request| { retry = Some(request); true });
        if attempt == 2 {
            assert!(retry.is_none(), "a page has a finite retry budget");
            break;
        }
        request = retry.unwrap();
    }
    assert_eq!(rks(&owner.state, 0), cards);
    assert!(owner.state.srcs[0].page.is_none());
    assert!(ask(&mut owner, false).is_some(), "new demand can retry the row");
    reset(&mut owner.state, &owner.adapter);
    plx_plex::plex::reset_servers_for_test();
}

#[test]
fn leaving_a_page_releases_another_row_and_rejects_the_abandoned_result() {
    let _guard = plx_base::testlock::serial();
    for failed in [false, true] {
        let mut owner = owner();
        let mut tv = page(0, 12, true).shelves.remove(0);
        tv.hub_id = "home.television.recent".into();
        tv.key = "/hubs/home/recentlyAdded?type=2".into();
        owner.state.srcs[0].last.as_mut().unwrap().shelves.push(tv);
        let old = ask(&mut owner, false).unwrap().complete(Some(page(0, 24, true)));
        if failed {
            let mut failure = old.clone();
            failure.build = None;
            let _ = super::land(&mut owner.state, &owner.adapter, &failure);
        }
        let _ = controlled_work(&mut owner.state, &owner.adapter,
            Some(crate::stores::hubs::HubsCmd::CancelPage { sid: sid(0), id: "home.movies.recent".into(),
                key: "/hubs/home/recentlyAdded?type=1".into() }), 0.0, &mut |_| panic!("cancel must not launch"));
        let mut next = None;
        let _ = request_page(&mut owner.state, &owner.adapter, sid(0), "home.television.recent",
            "/hubs/home/recentlyAdded?type=2", false, &BrowseScope::standalone(), &mut |request| { next = Some(request); true });
        let next = next.expect("another row can use the released source");
        let _ = super::land(&mut owner.state, &owner.adapter, &old);
        assert_eq!(owner.state.srcs[0].page.as_ref().unwrap().id, "home.television.recent");
        assert!(owner.state.srcs[0].fetching);
        let mut build = page(0,24,true);
        build.shelves[0].hub_id = "home.television.recent".into();
        build.shelves[0].key = "/hubs/home/recentlyAdded?type=2".into();
        deliver(&mut owner, next, Some(build));
        assert!(owner.state.srcs[0].page.is_none());
        assert_eq!(owner.state.srcs[0].last.as_ref().unwrap().shelves[1].items.len(),24);
        reset(&mut owner.state,&owner.adapter);
    }
    plx_plex::plex::reset_servers_for_test();
}

#[test]
fn paging_a_sparse_pinned_row_retains_its_selected_visible_card() {
    use std::io::{Read, Write};
    let _guard = plx_base::testlock::serial();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            let (mut socket, _) = match plx_base::testnet::accept(&listener) {
                Ok(socket) => socket,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(5)); continue;
                }
                Err(error) => panic!("pinned page fixture accept failed: {error}"),
            };
            socket.set_read_timeout(Some(std::time::Duration::from_secs(3))).unwrap();
            let mut request = [0;4096];
            let n = socket.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..n]);
            let start: usize = request.split("X-Plex-Container-Start=").nth(1).unwrap()
                .split('&').next().unwrap().parse().unwrap();
            let metadata: Vec<_> = (start..(start+24).min(240)).map(|i| serde_json::json!({
                "ratingKey":i.to_string(),"type":"movie","title":"Movie","thumb":"/poster",
                "librarySectionID":if i%8==0 { 7 } else { 8 }
            })).collect();
            let body=serde_json::json!({"MediaContainer":{"offset":start,"totalSize":240,"Metadata":metadata}}).to_string();
            write!(socket,"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
        }
    });
    plx_plex::plex::reset_servers_for_test();
    let registered=plx_plex::plex::register_for_test("pinned-http","127.0.0.1",i32::from(port),"synthetic","fixture");
    let mut owner=Owner::default();
    let mut build=page(0,24,true);
    for (i,item) in build.shelves[0].items.iter_mut().enumerate() {
        Arc::make_mut(item).sec=if i%8==0 { 7 } else { 8 };
    }
    seed(&mut owner.state,vec![src(registered.raw(),"",HubState::Ready,Some(build))]);
    let scope=BrowseScope { sections_gen:1,pins:vec![(registered,7,true),(registered,8,false)] };
    let mut held=None;
    let _=controlled_work_with_scope(&mut owner.state,&owner.adapter,
        Some(crate::stores::hubs::HubsCmd::Page { sid:registered,id:"home.movies.recent".into(),
            key:"/hubs/home/recentlyAdded?type=1".into(),before:false }),0.0,&scope,
        &mut |request| { held=Some(request);true });
    let request=held.unwrap();
    let result=request.fetch();
    let landing=request.complete(result);
    let _=step_landings_with_scope(&mut owner.state,&owner.adapter,None,||vec![landing],&scope,
        &mut |_| panic!("landing must not launch"));
    server.join().unwrap();
    let snapshot=hubs_snapshot(&owner.state);
    let hub=snapshot.view().hub(0).unwrap();
    assert!(hub.items.iter().any(|item| item.rk=="0"),"loading must retain the selected visible card");
    assert!(hub.items.iter().all(|item| item.sec==7));
    assert!(hub.items.len()<=24);
    reset(&mut owner.state,&owner.adapter);
    plx_plex::plex::reset_servers_for_test();
}

fn filtered_listing(start: usize, size: usize) -> plx_plex::plex::MediaContainer {
    let metadata: Vec<_> = (start..(start + size).min(500)).map(|i| serde_json::json!({
        "ratingKey":i.to_string(),"type":"movie","title":"Movie","thumb":"/poster",
        "librarySectionID":if i<12 || i>=300 { 7 } else { 8 }
    })).collect();
    serde_json::from_value(serde_json::json!({"offset":start,"totalSize":500,"Metadata":metadata})).unwrap()
}

#[test]
fn fully_hidden_pages_advance_in_both_directions_without_losing_focus() {
    let _guard = plx_base::testlock::serial();
    let mut current = page(0,12,true).shelves.remove(0);
    for item in &mut current.items { Arc::make_mut(item).sec=7; }
    let mut query = PageQuery { id:current.hub_id.clone(),key:current.key.clone(),start:12,before:false,hidden:vec![8],reload:false };
    let mut calls = Vec::new();
    current=fetch_window(sid(0),&query,Some(&current),0,|start,size| {
        calls.push(start);Some(filtered_listing(start,size))
    }).unwrap().shelves.remove(0);
    assert_eq!(calls,vec![11,36,60,84,108,132,156,180]);
    assert_eq!(current.end,204);
    assert_eq!(current.items.len(),12);
    assert!(current.items.iter().any(|item| item.rk=="5"));
    assert!(current.more);
    query.start=current.end;
    current=fetch_window(sid(0),&query,Some(&current),0,|start,size| Some(filtered_listing(start,size)))
        .unwrap().shelves.remove(0);
    assert_eq!(current.positions,vec![0,1,2,3,4,5,6,7,8,9,10,11,300,301,302,303,304,305,306,307,308,309,310,311]);
    assert_eq!(current.end,312);
    query.start=current.end;
    current=fetch_window(sid(0),&query,Some(&current),0,|start,size| Some(filtered_listing(start,size)))
        .unwrap().shelves.remove(0);
    assert_eq!(current.offset,300);
    query.before=true;
    query.start=current.offset;
    current=fetch_window(sid(0),&query,Some(&current),0,|start,size| Some(filtered_listing(start,size)))
        .unwrap().shelves.remove(0);
    assert_eq!(current.offset,204);
    assert_eq!(current.items.len(),12);
    assert!(current.items.iter().any(|item| item.rk=="305"));
    for expected in [108,12] {
        query.start=current.offset;
        current=fetch_window(sid(0),&query,Some(&current),0,|start,size| Some(filtered_listing(start,size)))
            .unwrap().shelves.remove(0);
        assert_eq!(current.offset,expected);
        assert_eq!(current.items.len(),12);
    }
    query.start=current.offset;
    current=fetch_window(sid(0),&query,Some(&current),0,|start,size| Some(filtered_listing(start,size)))
        .unwrap().shelves.remove(0);
    assert_eq!(current.offset,0);
    assert_eq!(current.items[17].rk,"305");
    assert_eq!(current.items.len(),24);
}

#[test]
fn refreshing_a_sparse_window_scans_its_existing_range() {
    let _guard = plx_base::testlock::serial();
    let query=PageQuery { id:"home.movies.recent".into(),key:"/hubs/recent".into(),
        start:0,before:false,hidden:vec![8],reload:false };
    let result=fetch_window(sid(0),&query,None,312,|start,size| Some(filtered_listing(start,size))).unwrap();
    let shelf=&result.shelves[0];
    assert_eq!(shelf.items.len(),24);
    assert_eq!(shelf.items[17].rk,"305");
    assert_eq!(shelf.end,312);
}

#[test]
fn observed_recently_added_response_shape_stops_at_the_short_last_page() {
    let _guard = plx_base::testlock::serial();
    let response = |start: usize, count: usize| {
        let metadata: Vec<_> = (start..start + count).map(|i| serde_json::json!({
            "ratingKey":i.to_string(),"type":"movie","title":"Movie","thumb":"/poster"
        })).collect();
        serde_json::from_value::<plx_plex::plex::MediaContainer>(serde_json::json!({
            "offset":start,"size":count,"totalSize":50,"Metadata":metadata
        })).unwrap()
    };
    let first = response(0,36);
    assert_eq!(first.offset,0);
    assert_eq!(first.metadata.len(),36);
    assert_eq!(first.total_size,50);
    let query = PageQuery { id:"home.movies.recent".into(),key:"/hubs/home/recentlyAdded?type=1".into(),
        start:36,before:false,hidden:Vec::new(),reload:false };
    let mut calls = Vec::new();
    let result = fetch_window(sid(0),&query,None,0,|start,size| {
        calls.push((start,size));
        Some(response(36,14))
    }).unwrap();
    let shelf = &result.shelves[0];
    assert_eq!(calls,vec![(36,24)]);
    assert_eq!(shelf.items.len(),14);
    assert_eq!(shelf.items[0].rk,"36");
    assert_eq!(shelf.end,50);
    assert!(!shelf.more);
}

fn walk_listing(start: usize, size: usize, total: usize, hidden_middle: bool) -> plx_plex::plex::MediaContainer {
    let metadata: Vec<_> = (start..(start + size).min(total)).map(|i| serde_json::json!({
        "ratingKey":i.to_string(),"type":"movie","title":"Movie","thumb":"/poster",
        "librarySectionID":if hidden_middle && (40..70).contains(&i) { 8 } else { 7 }
    })).collect();
    serde_json::from_value(serde_json::json!({"offset":start,"totalSize":total,"Metadata":metadata})).unwrap()
}

// Walks forward to a window near the end of the list, then backward to the start, asserting that
// every visible position is reached and no window exceeds the card cap.
fn check_backward_walk(hidden_middle: bool) {
    let _guard = plx_base::testlock::serial();
    const TOTAL: usize = 110;
    let hidden = if hidden_middle { vec![8] } else { Vec::new() };
    let visible = |p: usize| !(hidden_middle && (40..70).contains(&p));
    let mut query = PageQuery { id:"home.movies.recent".into(),key:"/hubs/home/recentlyAdded?type=1".into(),
        start:0,before:false,hidden,reload:false };
    let mut current = fetch_window(sid(0),&query,None,0,|start,size| Some(walk_listing(start,size,TOTAL,hidden_middle)))
        .unwrap().shelves.remove(0);
    while current.end < TOTAL {
        query.start = current.end;
        current = fetch_window(sid(0),&query,Some(&current),0,|start,size| Some(walk_listing(start,size,TOTAL,hidden_middle)))
            .unwrap().shelves.remove(0);
    }
    assert!(current.offset >= 84, "forward walk reached the tail window, got {}", current.offset);
    let mut seen: std::collections::BTreeSet<usize> = current.positions.iter().copied().collect();
    query.before = true;
    while current.offset > 0 {
        let previous_first = current.positions[0];
        query.start = current.offset;
        current = fetch_window(sid(0),&query,Some(&current),0,|start,size| Some(walk_listing(start,size,TOTAL,hidden_middle)))
            .unwrap().shelves.remove(0);
        assert!(current.items.len() <= MAX_SHELF_ITEMS);
        assert!(current.offset < query.start, "a backward page makes progress");
        for p in current.offset..previous_first {
            assert!(!visible(p) || current.positions.contains(&p), "position {p} skipped by a backward page");
        }
        seen.extend(current.positions.iter().copied());
    }
    let want: std::collections::BTreeSet<usize> = (0..TOTAL).filter(|p| visible(*p)).collect();
    assert_eq!(seen, want);
}

#[test]
fn walking_backward_visits_every_position() {
    check_backward_walk(false);
}

#[test]
fn walking_backward_over_a_hidden_section_visits_every_visible_position() {
    check_backward_walk(true);
}

#[test]
fn a_failed_window_reload_keeps_the_first_page_instead_of_failing_the_refresh() {
    use std::io::{Read, Write};
    let _guard = plx_base::testlock::serial();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        let mut served = 0;
        while std::time::Instant::now() < deadline && served < 3 {
            let (mut socket, _) = match plx_base::testnet::accept(&listener) {
                Ok(socket) => socket,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                    continue;
                }
                Err(error) => panic!("window failure fixture accept failed: {error}"),
            };
            socket.set_read_timeout(Some(std::time::Duration::from_secs(3))).unwrap();
            let mut request = [0; 4096];
            let n = socket.read(&mut request).unwrap();
            let path = String::from_utf8_lossy(&request[..n]).split_whitespace().nth(1).unwrap().to_owned();
            let (status, body) = if path.starts_with("/hubs/home/recentlyAdded?") {
                ("500 Internal Server Error", String::new())
            } else if path.starts_with("/hubs/continueWatching?") {
                ("200 OK", serde_json::json!({"MediaContainer":{"Hub":[]}}).to_string())
            } else {
                let metadata: Vec<_> = (0..12).map(|i| serde_json::json!({
                    "ratingKey":i.to_string(),"type":"movie","title":"Fresh movie","thumb":"/poster"})).collect();
                ("200 OK", serde_json::json!({"MediaContainer":{"Hub":[{"hubIdentifier":"home.movies.recent",
                    "type":"movie","title":"Recent","key":"/hubs/home/recentlyAdded?type=1",
                    "more":true,"Metadata":metadata}]}}).to_string())
            };
            write!(socket,"HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
            served += 1;
        }
    });
    plx_plex::plex::reset_servers_for_test();
    let registered = plx_plex::plex::register_for_test("window-fail-http", "127.0.0.1", i32::from(port), "synthetic", "fixture");
    let mut owner = Owner::default();
    seed(&mut owner.state, vec![src(registered.raw(), "", HubState::Ready, Some(page(36,24,true)))]);
    let mut held = None;
    let _ = request_refetch_hubs_with_scope(&mut owner.state, &owner.adapter, &BrowseScope::standalone(),
        &mut |request| { held = Some(request); true });
    let request = held.unwrap();
    assert!(!request.windows.is_empty(), "the refresh must try to reload the paged window");
    let result = request.fetch();
    server.join().unwrap();
    let build = result.expect("a failed window reload must not fail the source refresh");
    assert_eq!(build.shelves[0].items[0].title, "Fresh movie");
    assert_eq!(build.shelves[0].offset, 0);
    assert_eq!(build.shelves[0].items.len(), 12);
    reset(&mut owner.state,&owner.adapter);
    plx_plex::plex::reset_servers_for_test();
}

// The window one level down, so a test can read the re-anchor flag beside its rows.
fn window_info(shelf: &Shelf) -> paging::PageInfo {
    paging::PageInfo { offset: shelf.offset, end: shelf.end, total: shelf.total, more: shelf.more, unstable: false }
}

fn window_rows(shelf: &Shelf) -> Vec<paging::Row> {
    shelf.positions.iter().copied().zip(shelf.items.iter().map(Arc::clone)).collect()
}

fn row_keys(rows: &[paging::Row]) -> Vec<String> {
    rows.iter().map(|(_, item)| item.rk.clone()).collect()
}

fn row_positions(rows: &[paging::Row]) -> Vec<usize> {
    rows.iter().map(|(position, _)| *position).collect()
}

fn step(rows: &[paging::Row], info: paging::PageInfo, start: usize, before: bool, hidden: &[i64],
    fetch: impl FnMut(usize, usize) -> Option<plx_plex::plex::MediaContainer>) -> (Vec<paging::Row>, paging::PageInfo) {
    paging::fetch_window(sid(0), &paging::Ask { start, before, hidden }, Some((rows, info)), 0, fetch).unwrap()
}

// Rows 12..=35 of the untouched listing, read as the window a backward ask starts from.
fn first_window(start: usize) -> Shelf {
    fetch_window(sid(0), &recent_query(start), None, 0, serve(|p| p, 100, Some(100), 0)).unwrap().shelves.remove(0)
}

#[test]
fn a_forward_edge_that_moved_more_than_the_retained_half_is_found_on_the_other_side() {
    let _guard = plx_base::testlock::serial();
    // Rows 60..=83 are read. A forward ask keeps the last twelve, 72..=83, so 83 is the edge.
    let first = first_window(60);
    // Twenty rows before the window are removed, so every key now sits twenty positions lower:
    // key 83 is at position 63, outside the page the ask reads from 83 but inside the page before it.
    let mut shifted = serve(|p| p + 20, 200, Some(200), 0);
    let (rows, info) = step(&window_rows(&first), window_info(&first), first.end, false, &[], &mut shifted);
    assert!(!info.unstable);
    assert_eq!(row_keys(&rows), key_range(72..=95), "the rows after the edge are read, with none skipped");
    assert_eq!(row_positions(&rows), (52..76).collect::<Vec<_>>());
    assert_eq!((info.offset, info.end), (52, 76));
}

#[test]
fn a_forward_edge_missing_from_both_reads_returns_the_window_unchanged_and_unstable() {
    let _guard = plx_base::testlock::serial();
    let first = first_window(0);
    // Key 23, the edge, is removed; no page of the listing holds it any more.
    let mut removed = serve(|p| if p < 23 { p } else { p + 1 }, 100, Some(100), 0);
    let (rows, info) = step(&window_rows(&first), window_info(&first), first.end, false, &[], &mut removed);
    assert!(info.unstable, "the edge was not found, so the window is flagged");
    assert_eq!(row_keys(&rows), key_range(0..=23), "nothing held is discarded");
    assert_eq!(row_positions(&rows), (0..24).collect::<Vec<_>>());
    assert_eq!((info.offset, info.end, info.more), (0, 24, first.more));
}

#[test]
fn a_backward_ask_re_anchors_on_the_edge_after_an_item_is_added_at_the_head() {
    let _guard = plx_base::testlock::serial();
    let first = first_window(12);
    // Between the asks one item lands at the head, so every position moves up by one: key 12 is at 13.
    let mut shifted = serve(|p| if p == 0 { 999 } else { p - 1 }, 100, Some(100), 0);
    let (rows, info) = step(&window_rows(&first), window_info(&first), first.offset, true, &[], &mut shifted);
    assert!(!info.unstable);
    assert_eq!(row_keys(&rows), key_range(0..=23), "the cards before the edge are read, with none repeated");
    assert_eq!(row_positions(&rows), (1..=24).collect::<Vec<_>>());
    assert_eq!((info.offset, info.end), (1, 25));
}

#[test]
fn a_backward_ask_re_anchors_on_the_edge_after_an_earlier_item_is_removed() {
    let _guard = plx_base::testlock::serial();
    let first = first_window(12);
    // Key 5 is removed, so the edge, key 12, moves down to position 11 and the page at 0..=12 holds it.
    let mut shifted = serve(|p| if p < 5 { p } else { p + 1 }, 100, Some(100), 0);
    let (rows, info) = step(&window_rows(&first), window_info(&first), first.offset, true, &[], &mut shifted);
    assert!(!info.unstable);
    let mut expected = key_range(0..=4);
    expected.extend(key_range(6..=23));
    assert_eq!(row_keys(&rows), expected, "key 11, the card before the edge, is read; removed key 5 is not");
    assert_eq!(row_positions(&rows), (0..23).collect::<Vec<_>>());
    assert_eq!(info.offset, 0);
}

#[test]
fn a_gapped_ask_verifies_the_edge_where_it_stands_and_reads_past_the_hidden_rows() {
    let _guard = plx_base::testlock::serial();
    // Visible rows 0..=11 at positions 0..=11, then hidden rows (section 8) up to position 60.
    let mut first = page(0, 12, true).shelves.remove(0);
    for item in &mut first.items { Arc::make_mut(item).sec = 7; }
    let info = paging::PageInfo { end: 60, total: 500, ..window_info(&first) };
    // Between the asks one item lands at the head: key 11 moves from 11 to 12.
    let shifted = |start: usize, size: usize| {
        let metadata: Vec<_> = (start..(start + size).min(500)).map(|p| {
            let key = if p == 0 { 999 } else { p - 1 };
            serde_json::json!({"ratingKey": key.to_string(), "type": "movie", "title": "Movie", "thumb": "/poster",
                "librarySectionID": if key < 12 || key >= 300 { 7 } else { 8 }})
        }).collect();
        Some(serde_json::from_value::<plx_plex::plex::MediaContainer>(
            serde_json::json!({"offset": start, "totalSize": 500, "Metadata": metadata})).unwrap())
    };
    let (rows, info) = step(&window_rows(&first), info, 60, false, &[8], shifted);
    assert!(!info.unstable);
    assert_eq!(row_keys(&rows), key_range(0..=11), "the visible rows keep their keys");
    assert_eq!(row_positions(&rows), (1..=12).collect::<Vec<_>>(), "and move up with the edge");
    assert_eq!((info.offset, info.end), (1, 253), "the reads continue from the window's end, not from the edge");
}

// Characterisation of the window's edge cases, on today's code. Each test feeds `fetch_window` a
// closure in place of the server, so the response shape is the only thing that varies.

fn recent_query(start: usize) -> PageQuery {
    PageQuery { id: "home.movies.recent".into(), key: "/hubs/home/recentlyAdded?type=1".into(), start, before: false,
        hidden: Vec::new(), reload: false }
}

// A listing of `len` rows whose row at server position `p` has rating key `key_at(p)`. `total` is
// the response's `totalSize` (omitted when `None`); `overfill` adds rows beyond the page asked for.
fn serve(key_at: impl Fn(usize) -> usize, len: usize, total: Option<i64>, overfill: usize)
    -> impl FnMut(usize, usize) -> Option<plx_plex::plex::MediaContainer> {
    move |start, size| {
        let metadata: Vec<_> = (start..(start + size + overfill).min(len)).map(|p| serde_json::json!({
            "ratingKey": key_at(p).to_string(), "type": "movie", "title": "Movie", "thumb": "/poster"
        })).collect();
        let mut container = serde_json::json!({"offset": start, "Metadata": metadata});
        if let Some(total) = total { container["totalSize"] = total.into(); }
        Some(serde_json::from_value(container).unwrap())
    }
}

fn window_keys(shelf: &Shelf) -> Vec<String> {
    shelf.items.iter().map(|item| item.rk.clone()).collect()
}

fn key_range(keys: std::ops::RangeInclusive<usize>) -> Vec<String> {
    keys.map(|k| k.to_string()).collect()
}

#[test]
fn a_paged_response_without_total_size_is_read_from_its_page_size_alone() {
    let _guard = plx_base::testlock::serial();
    let mut listing = serve(|p| p, 30, None, 0);
    let mut query = recent_query(0);
    let first = fetch_window(sid(0), &query, None, 0, &mut listing).unwrap().shelves.remove(0);
    // Without a total, a full page is "more" and a short page is the end.
    assert_eq!(first.items.len(), 24);
    assert_eq!(first.total, 0);
    assert_eq!((first.offset, first.end), (0, 24));
    assert!(first.more);
    let mut calls = Vec::new();
    query.start = first.end;
    let second = fetch_window(sid(0), &query, Some(&first), 0, |start, size| {
        calls.push((start, size));
        listing(start, size)
    }).unwrap().shelves.remove(0);
    assert_eq!(calls, vec![(23, 25)], "the ask starts at the retained edge row, 23");
    assert_eq!(second.positions, (12..30).collect::<Vec<_>>());
    assert_eq!(window_keys(&second), key_range(12..=29));
    assert_eq!((second.offset, second.end), (12, 30));
    assert!(!second.more);
}

#[test]
fn a_server_returning_more_rows_than_asked_keeps_only_the_page_asked_for() {
    let _guard = plx_base::testlock::serial();
    let mut listing = serve(|p| p, 200, Some(100), 16);
    let mut calls = Vec::new();
    let shelf = fetch_window(sid(0), &recent_query(0), None, 0, |start, size| {
        calls.push((start, size));
        listing(start, size)
    }).unwrap().shelves.remove(0);
    assert_eq!(calls, vec![(0, 24)], "the surplus rows are not read as a second page");
    assert_eq!(window_keys(&shelf), key_range(0..=23));
    assert_eq!(shelf.positions, (0..24).collect::<Vec<_>>());
    assert_eq!((shelf.offset, shelf.end, shelf.total), (0, 24, 100));
    assert!(shelf.more);
}

#[test]
fn a_response_echoing_a_different_offset_drops_the_whole_page() {
    let _guard = plx_base::testlock::serial();
    let mut listing = serve(|p| p, 100, Some(100), 0);
    let query = recent_query(0);
    let first = fetch_window(sid(0), &query, None, 0, &mut listing).unwrap().shelves.remove(0);
    let mut calls = Vec::new();
    // A server that ignores paging answers every ask from its first row.
    let ignored = fetch_window(sid(0), &PageQuery { start: first.end, ..recent_query(0) }, Some(&first), 0, |start, size| {
        calls.push((start, size));
        serve(|p| p, 100, Some(100), 0)(0, size)
    });
    assert!(ignored.is_none(), "an echoed offset other than the request fails the page");
    assert_eq!(calls, vec![(23, 25)], "the mismatch is not retried inside one page");
}

#[test]
fn an_item_added_at_the_head_between_asks_shifts_the_window_without_a_duplicate() {
    let _guard = plx_base::testlock::serial();
    let first = fetch_window(sid(0), &recent_query(0), None, 0, serve(|p| p, 100, Some(100), 0))
        .unwrap().shelves.remove(0);
    // Between the asks the listing gains one row at position 0, so every later position moves by one.
    let mut shifted = serve(|p| if p == 0 { 999 } else { p - 1 }, 100, Some(100), 0);
    let second = fetch_window(sid(0), &PageQuery { start: first.end, ..recent_query(0) }, Some(&first), 0, &mut shifted)
        .unwrap().shelves.remove(0);
    // The ask's page starts at the edge, key 23. It finds the edge at position 24, so the retained
    // rows move up by one and the fresh rows continue from key 24 at position 25.
    assert_eq!(window_keys(&second), key_range(12..=35));
    let unique: std::collections::BTreeSet<_> = window_keys(&second).into_iter().collect();
    assert_eq!(unique.len(), 24, "no card appears twice");
    assert!(!unique.contains("999"), "the head item lies before the window and is not read");
    assert_eq!(second.positions, (13..=36).collect::<Vec<_>>());
    assert_eq!((second.offset, second.end), (13, 37));
    assert!(second.more);
}

#[test]
fn a_held_item_removed_between_asks_loses_the_item_after_it() {
    let _guard = plx_base::testlock::serial();
    let first = fetch_window(sid(0), &recent_query(0), None, 0, serve(|p| p, 100, Some(100), 0))
        .unwrap().shelves.remove(0);
    // Key 20 is removed from the listing, so every later row moves up by one. The edge, key 23, is
    // now at position 22: the page at 23 misses it, and the one page before 23 finds it.
    let mut shifted = serve(|p| if p < 20 { p } else { p + 1 }, 100, Some(100), 0);
    let second = fetch_window(sid(0), &PageQuery { start: first.end, ..recent_query(0) }, Some(&first), 0, &mut shifted)
        .unwrap().shelves.remove(0);
    let keys = window_keys(&second);
    assert!(keys.contains(&"20".to_string()), "the window keeps the removed card until it is paged out");
    assert!(keys.contains(&"24".to_string()), "key 24, the row after the edge, is read");
    assert_eq!(keys, key_range(12..=35));
    assert_eq!((second.offset, second.end), (11, 35));
    assert!(second.more);
}

#[test]
fn a_page_of_only_held_rows_adds_nothing_and_stops_at_the_request_bound() {
    let _guard = plx_base::testlock::serial();
    let first = fetch_window(sid(0), &recent_query(0), None, 0, serve(|p| p, 100, Some(100), 0))
        .unwrap().shelves.remove(0);
    // Every page from here on repeats rows 12..=23, which the retained half already holds. The first
    // ask's page starts at the edge, row 23, which matches, so its overlap row is not added twice.
    let mut calls = Vec::new();
    let shelf = fetch_window(sid(0), &PageQuery { start: first.end, ..recent_query(0) }, Some(&first), 0, |start, size| {
        calls.push((start, size));
        serve(|p| 12 + p % 12, 10_000, Some(10_000), 0)(start, size)
    }).unwrap().shelves.remove(0);
    assert_eq!(calls.len(), 8, "the request bound ends the walk");
    assert_eq!(calls[0], (23, 25));
    assert_eq!(window_keys(&shelf), key_range(12..=23), "a duplicate is never shown twice");
    assert_eq!(shelf.items.len(), 12, "the window stays short rather than filling with repeats");
    assert_eq!((shelf.offset, shelf.end), (12, 24 + 8 * 24));
    assert!(shelf.more);
}
