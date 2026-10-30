use super::*;

/// Canned answers by path prefix, recording every request.
struct Fake {
    routes: Vec<(&'static str, u16, &'static str)>,
    log: Mutex<Vec<(String, Verb, Option<String>, Vec<String>)>>,
}

impl Transport for std::sync::Arc<Fake> {
    fn send(&self, url: &str, verb: Verb, headers: &[String], body: Option<&[u8]>) -> Option<Answer> {
        let path = url.strip_prefix("http://jf:8096").expect("every request goes to the base");
        self.log.lock().unwrap().push((
            path.to_string(),
            verb,
            body.map(|b| String::from_utf8_lossy(b).into_owned()),
            headers.to_vec(),
        ));
        let (_, status, body) = self.routes.iter().find(|(p, ..)| path.starts_with(p))?;
        Some(Answer { status: *status, body: body.as_bytes().to_vec() })
    }
}

fn client(routes: Vec<(&'static str, u16, &'static str)>) -> (JfClient, std::sync::Arc<Fake>) {
    let fake = std::sync::Arc::new(Fake { routes, log: Mutex::new(Vec::new()) });
    let id = ClientIdentity {
        client: "FinNative".into(),
        device: "LG C1".into(),
        device_id: "dev1".into(),
        version: "0.1.0".into(),
    };
    (JfClient::new("jf:8096/", id, Box::new(fake.clone())), fake)
}

const AUTH: &str = r#"{"User":{"Id":"5F0A-11","Name":"alice"},"AccessToken":"tok","ServerId":"SRV"}"#;

/// Only the tokenless calls go out without a credential: the server's public description and
/// starting Quick Connect. The password, and everything after sign-in, is one — so the real
/// transport puts each of those through the credential authority.
#[test]
fn only_the_tokenless_calls_carry_no_credential() {
    let (mut c, fake) = client(vec![
        ("/System/Info/Public", 200, r#"{"Id":"SRV","ServerName":"Home"}"#),
        ("/QuickConnect/Initiate", 200, r#"{"Code":"123456","Secret":"s"}"#),
        ("/Users/AuthenticateByName", 200, AUTH),
        ("/UserViews", 200, r#"{"Items":[]}"#),
    ]);
    c.probe().expect("a server");
    c.quick_connect_start().expect("a code");
    c.sign_in("alice", "pw").expect("signed in");
    let _ = c.libraries();
    let log = fake.log.lock().unwrap();
    let carried: Vec<(String, bool)> = log
        .iter()
        .map(|(path, _, body, headers)| {
            (path.clone(), carries_credential(&format!("http://jf:8096{path}"), headers, body.is_some()))
        })
        .collect();
    assert_eq!(carried[0], ("/System/Info/Public".into(), false));
    assert_eq!(carried[1], ("/QuickConnect/Initiate".into(), false));
    assert_eq!(carried[2], ("/Users/AuthenticateByName".into(), true), "the password");
    assert!(carried[3..].iter().all(|(_, c)| *c), "signed in: {carried:?}");
    assert!(carries_credential("http://jf:8096/Videos/m1/stream?static=true&ApiKey=tok", &[], false));
    assert!(!carries_credential(&format!("http://jf:8096{}", c.image_path("/library/metadata/m1/thumb", 300, 450)), &[], false));
}

#[test]
fn base_addresses_are_normalized() {
    assert_eq!(normalize_base("192.168.1.20:8096/"), "http://192.168.1.20:8096");
    assert_eq!(normalize_base("192.168.1.20"), "http://192.168.1.20:8096");
    assert_eq!(normalize_base(" https://media.example/jellyfin/ "), "https://media.example:443/jellyfin");
    assert_eq!(normalize_base("HTTP://nas.local"), "http://nas.local:80");
    assert_eq!(normalize_base("http://[fd00::5]:8096"), "http://[fd00::5]:8096");
    assert_eq!(normalize_base("[fd00::5]"), "http://[fd00::5]:8096");
    assert_eq!(base_path("https://media.example:443/jellyfin"), "/jellyfin");
    assert_eq!(base_path("http://nas.local:8096"), "");
}

#[test]
fn plex_shaped_image_paths_become_jellyfin_images() {
    assert_eq!(plex_image_path("/library/metadata/abc/clearLogo").as_deref(), Some("/Items/abc/Images/Logo?quality=90"));
    assert_eq!(plex_image_path("/library/metadata/abc/thumb/123").as_deref(), Some("/Items/abc/Images/Primary?quality=90"));
    assert_eq!(plex_image_path("/Items/abc/Images/Primary?tag=x"), None);
    // A tag the server gave is in the request, so a replaced image is a new request.
    image_tags::note("tagged", paths::ImageKind::Logo, "l2");
    assert_eq!(plex_image_path("/library/metadata/tagged/clearLogo").as_deref(), Some("/Items/tagged/Images/Logo?quality=90&tag=l2"));
}

#[test]
fn probe_tells_jellyfin_from_anything_else() {
    let (c, _) = client(vec![("/System/Info/Public", 200, r#"{"ServerName":"Home","Version":"10.10.7","Id":"abc"}"#)]);
    assert_eq!(c.probe().unwrap().server_name, "Home");
    let (c, _) = client(vec![("/System/Info/Public", 200, "<html>router</html>")]);
    assert_eq!(c.probe().unwrap_err(), SignInError::NotJellyfin);
    let (c, _) = client(vec![]);
    assert_eq!(c.probe().unwrap_err(), SignInError::Unreachable);
}

#[test]
fn password_sign_in_adopts_the_token_and_sends_it_after() {
    let (mut c, fake) = client(vec![
        ("/Users/AuthenticateByName", 200, AUTH),
        ("/UserViews", 200, r#"{"Items":[]}"#),
    ]);
    let s = c.sign_in("alice", "pw").unwrap();
    assert_eq!((s.user_id.as_str(), s.token.as_str(), s.server_id.as_str()), ("5f0a11", "tok", "srv"));
    c.libraries().unwrap();
    let log = fake.log.lock().unwrap();
    let (path, verb, body, headers) = &log[0];
    assert_eq!((path.as_str(), *verb), ("/Users/AuthenticateByName", Verb::Post));
    assert_eq!(body.as_deref(), Some(r#"{"Pw":"pw","Username":"alice"}"#));
    assert!(!headers[0].contains("Token="), "no token before sign-in");
    assert!(headers.contains(&"Content-Type: application/json".to_string()));
    let (path, _, _, headers) = &log[1];
    assert_eq!(path, "/UserViews?userId=5f0a11");
    assert!(headers[0].ends_with(r#"Token="tok""#), "{}", headers[0]);
}

#[test]
fn wrong_password_is_rejected_not_unreachable() {
    let (mut c, _) = client(vec![("/Users/AuthenticateByName", 401, "")]);
    assert_eq!(c.sign_in("alice", "nope").unwrap_err(), SignInError::Rejected);
    assert!(c.session().is_none());
}

#[test]
fn quick_connect_waits_then_signs_in() {
    let (c, _) = client(vec![("/QuickConnect/Initiate", 401, "")]);
    assert_eq!(c.quick_connect_start().unwrap_err(), SignInError::QuickConnectDisabled);

    let (mut c, _) = client(vec![
        ("/QuickConnect/Initiate", 200, r#"{"Secret":"s3","Code":"482913","Authenticated":false}"#),
        ("/QuickConnect/Connect", 200, r#"{"Secret":"s3","Code":"482913","Authenticated":false}"#),
    ]);
    let qc = c.quick_connect_start().unwrap();
    assert_eq!(qc.code, "482913");
    assert_eq!(c.quick_connect_poll(&qc.secret).unwrap(), None);

    let (mut c, fake) = client(vec![
        ("/QuickConnect/Connect", 200, r#"{"Secret":"s3","Code":"482913","Authenticated":true}"#),
        ("/Users/AuthenticateWithQuickConnect", 200, AUTH),
    ]);
    let s = c.quick_connect_poll("s3").unwrap().unwrap();
    assert_eq!(s.token, "tok");
    assert_eq!(fake.log.lock().unwrap()[1].2.as_deref(), Some(r#"{"Secret":"s3"}"#));
}

fn signed_in(routes: Vec<(&'static str, u16, &'static str)>) -> (JfClient, std::sync::Arc<Fake>) {
    let (mut c, f) = client(routes);
    c.session = Some(Session { user_id: "u1".into(), user_name: "t".into(), token: "tok".into(), server_id: "s".into(), ..Default::default() });
    (c, f)
}

const VIEWS: &str = r#"{"Items":[
    {"Id":"lib-m","Name":"Movies","Type":"CollectionFolder","CollectionType":"movies"},
    {"Id":"lib-t","Name":"Shows","Type":"CollectionFolder","CollectionType":"tvshows"},
    {"Id":"lib-x","Name":"Music","Type":"CollectionFolder","CollectionType":"music"}]}"#;

#[test]
fn home_has_one_recent_shelf_per_video_library_and_leaves_the_deck_to_its_own_read() {
    let (c, fake) = signed_in(vec![
        ("/UserViews", 200, VIEWS),
        ("/UserItems/Resume", 200, r#"{"Items":[{"Id":"m1","Name":"Half Watched","Type":"Movie"}],"TotalRecordCount":1}"#),
        ("/Shows/NextUp", 200, r#"{"Items":[],"TotalRecordCount":0}"#),
        ("/Items/Latest?userId=u1&parentId=libm", 200, r#"[{"Id":"m2","Name":"New Film","Type":"Movie"}]"#),
        ("/Items/Latest?userId=u1&parentId=libt", 500, ""),
    ]);
    let home = c.home_hubs().unwrap();
    let shelves: Vec<_> = home.hub.iter().map(|h| (h.hub_identifier.as_str(), h.title.as_str(), h.metadata.len())).collect();
    assert_eq!(
        shelves,
        [("home.movies.recent", "Recently Added in Movies", 1)],
        "the failing TV shelf is left out; music has no shelf"
    );
    assert_eq!(home.hub[0].metadata[0].library_section_id, 1);
    let log = fake.log.lock().unwrap();
    assert!(!log.iter().any(|(p, ..)| p.contains("parentId=libx")));
    assert!(
        !log.iter().any(|(p, ..)| p.starts_with("/UserItems/Resume") || p.starts_with("/Shows/NextUp")),
        "Continue Watching comes from its own read, never from the hub list"
    );
}

/// The user's favourites are a Home shelf of their own, before the libraries.
#[test]
fn home_has_a_favorites_shelf_before_the_libraries() {
    let (c, fake) = signed_in(vec![
        ("/UserViews", 200, VIEWS),
        ("/UserItems/Resume", 200, r#"{"Items":[]}"#),
        ("/Shows/NextUp", 200, r#"{"Items":[{"Id":"e1","Name":"Ep","Type":"Episode"}]}"#),
        ("/Items?userId=u1", 200, r#"{"Items":[{"Id":"m9","Name":"Loved","Type":"Movie","UserData":{"IsFavorite":true}}],"TotalRecordCount":30}"#),
        ("/Items/Latest?userId=u1&parentId=libm", 200, r#"[{"Id":"m2","Name":"New Film","Type":"Movie"}]"#),
        ("/Items/Latest?userId=u1&parentId=libt", 500, ""),
    ]);
    let home = c.home_hubs().unwrap();
    let shelves: Vec<_> = home.hub.iter().map(|h| (h.hub_identifier.as_str(), h.title.as_str(), h.key.as_str())).collect();
    assert_eq!(
        shelves,
        [
            ("home.favorites", "Favorites", "/jf/favorites"),
            ("home.movies.recent", "Recently Added in Movies", "/jf/latest/1"),
        ]
    );
    assert!(home.hub[0].more, "30 favourites: the shelf offers the rest");
    let log = fake.log.lock().unwrap();
    let asked = log.iter().map(|(p, ..)| p.as_str()).find(|p| p.starts_with("/Items?")).expect("favourites asked for");
    assert!(asked.contains("filters=IsFavorite"), "{asked}");
    assert!(asked.contains("includeItemTypes=Movie%2CSeries%2CEpisode") || asked.contains("includeItemTypes=Movie,Series,Episode"), "{asked}");
    assert!(asked.contains("recursive=true"), "{asked}");
}

/// Home follows the user's Jellyfin settings: `OrderedViews` orders the Latest shelves,
/// `LatestItemsExcludes` drops a library's, and `HidePlayedInLatest` asks for unplayed items only.
#[test]
fn home_follows_the_users_view_order_exclusions_and_hide_played() {
    let (c, fake) = signed_in(vec![
        ("/Users/u1", 200, r#"{"Id":"u1","Configuration":{"HidePlayedInLatest":true,"LatestItemsExcludes":["LIB-Y"],"OrderedViews":["lib-t","lib-m"],"MyMediaExcludes":[]}}"#),
        (
            "/UserViews",
            200,
            r#"{"Items":[
    {"Id":"lib-m","Name":"Movies","Type":"CollectionFolder","CollectionType":"movies"},
    {"Id":"lib-y","Name":"Kids","Type":"CollectionFolder","CollectionType":"movies"},
    {"Id":"lib-t","Name":"Shows","Type":"CollectionFolder","CollectionType":"tvshows"}]}"#,
        ),
        ("/UserItems/Resume", 200, r#"{"Items":[]}"#),
        ("/Shows/NextUp", 200, r#"{"Items":[]}"#),
        ("/Items/Latest?userId=u1&parentId=libm", 200, r#"[{"Id":"m2","Name":"New Film","Type":"Movie"}]"#),
        ("/Items/Latest?userId=u1&parentId=libt", 200, r#"[{"Id":"s2","Name":"New Show","Type":"Series"}]"#),
        ("/Items/Latest?userId=u1&parentId=liby", 200, r#"[{"Id":"x1","Name":"Cartoon","Type":"Movie"}]"#),
    ]);
    let ids: Vec<_> = c.home_hubs().unwrap().hub.iter().map(|h| h.hub_identifier.clone()).collect();
    assert_eq!(ids, ["home.television.recent", "home.movies.recent"], "the user's view order");
    let log = fake.log.lock().unwrap();
    let latest: Vec<_> = log.iter().map(|(p, ..)| p.as_str()).filter(|p| p.starts_with("/Items/Latest")).collect();
    assert_eq!(latest.len(), 2, "the excluded library is not asked for: {latest:?}");
    assert!(latest.iter().all(|p| p.contains("isPlayed=false")), "{latest:?}");
}

/// With `HidePlayedInLatest` off, Latest asks for played items too.
#[test]
fn latest_keeps_played_items_when_the_user_does_not_hide_them() {
    let (c, fake) = signed_in(vec![
        ("/Users/u1", 200, r#"{"Configuration":{"HidePlayedInLatest":false}}"#),
        ("/UserViews", 200, VIEWS),
        ("/Items/Latest", 200, r#"[]"#),
    ]);
    c.libraries();
    c.latest(1, 10);
    let log = fake.log.lock().unwrap();
    assert!(log.iter().any(|(p, ..)| p.starts_with("/Items/Latest") && !p.contains("isPlayed")), "{log:?}");
}

/// A transport that takes `delay` per request, counting how many are in flight at once.
struct Slow {
    inner: std::sync::Arc<Fake>,
    delay: std::time::Duration,
    now: std::sync::atomic::AtomicUsize,
    most: std::sync::atomic::AtomicUsize,
}

impl Transport for std::sync::Arc<Slow> {
    fn send(&self, url: &str, verb: Verb, headers: &[String], body: Option<&[u8]>) -> Option<Answer> {
        use std::sync::atomic::Ordering::SeqCst;
        let n = self.now.fetch_add(1, SeqCst) + 1;
        self.most.fetch_max(n, SeqCst);
        std::thread::sleep(self.delay);
        self.now.fetch_sub(1, SeqCst);
        self.inner.send(url, verb, headers, body)
    }
}

/// Home's shelves and a page's parts go out together, not one after another.
#[test]
fn home_and_pages_ask_for_their_parts_at_once() {
    let (mut c, fake) = signed_in(vec![
        ("/UserViews", 200, VIEWS),
        ("/UserItems/Resume", 200, r#"{"Items":[]}"#),
        ("/Shows/NextUp", 200, r#"{"Items":[]}"#),
        ("/Items/Latest", 200, r#"[{"Id":"m2","Name":"New","Type":"Movie"}]"#),
        ("/Items/m1", 200, r#"{"Id":"m1","Name":"Film","Type":"Movie"}"#),
        ("/MediaSegments/m1", 200, r#"{"Items":[]}"#),
    ]);
    let slow = std::sync::Arc::new(Slow {
        inner: fake,
        delay: std::time::Duration::from_millis(50),
        now: Default::default(),
        most: Default::default(),
    });
    c.transport = Box::new(slow.clone());
    use std::sync::atomic::Ordering::SeqCst;
    assert_eq!(c.home_hubs().unwrap().hub.len(), 2, "two recently-added shelves");
    // The library list, Continue Watching and Next Up were in flight together (one after
    // another would never have more than one).
    assert!(slow.most.load(SeqCst) >= 3, "{}", slow.most.load(SeqCst));
    slow.most.store(0, SeqCst);
    assert_eq!(c.metadata("m1").unwrap().title, "Film");
    assert_eq!(slow.most.load(SeqCst), 2, "the item and its segments together");
}

/// Shelf titles are the app's, in the viewer's language, not English from this layer.
#[test]
fn home_shelf_titles_follow_the_ui_language() {
    let _pt = plx_platform::i18n::language_on_this_thread_for_test(plx_platform::i18n::Preference::Pt);
    let (c, _) = signed_in(vec![
        ("/UserViews", 200, VIEWS),
        ("/Items?userId=u1", 200, r#"{"Items":[{"Id":"m9","Name":"Loved","Type":"Movie","UserData":{"IsFavorite":true}}]}"#),
        ("/Items/Latest?userId=u1&parentId=libm", 200, r#"[{"Id":"m2","Name":"New Film","Type":"Movie"}]"#),
        ("/Items/Latest?userId=u1&parentId=libt", 500, ""),
    ]);
    let titles: Vec<_> = c.home_hubs().unwrap().hub.into_iter().map(|h| h.title).collect();
    assert_eq!(titles, ["Favoritos", "Adicionados recentemente em Movies"]);
}

#[test]
fn a_librarys_collections_are_listed_from_the_whole_server() {
    let (c, fake) = signed_in(vec![
        ("/UserViews", 200, VIEWS),
        ("/Items?", 200, r#"{"Items":[{"Id":"b1","Name":"Toy Story Collection","Type":"BoxSet","ChildCount":4}],"TotalRecordCount":1}"#),
    ]);
    c.libraries().unwrap();
    let page = c
        .library_items(
            2,
            &LibraryPage {
                item_types: "BoxSet",
                sort_by: SortBy::Title,
                descending: false,
                unwatched_only: false,
                starts_with: None,
                genre_id: None,
                start: 0,
                size: 40,
            },
        )
        .unwrap();
    let row = &page.metadata[0];
    assert_eq!((row.kind.as_str(), row.child_count, row.library_section_id), ("collection", 4, 2));
    let log = fake.log.lock().unwrap();
    assert!(log[1].0.contains("includeItemTypes=BoxSet"), "{}", log[1].0);
    assert!(!log[1].0.contains("parentId="), "a collection is not under its members' library: {}", log[1].0);
}

#[test]
fn a_collections_count_probe_asks_for_the_total_and_no_rows() {
    let (c, fake) = signed_in(vec![
        ("/UserViews", 200, VIEWS),
        ("/Items?", 200, r#"{"Items":[],"TotalRecordCount":7}"#),
    ]);
    c.libraries().unwrap();
    let page = c
        .library_items(
            2,
            &LibraryPage {
                item_types: "BoxSet",
                sort_by: SortBy::Title,
                descending: false,
                unwatched_only: false,
                starts_with: None,
                genre_id: None,
                start: 0,
                size: 0,
            },
        )
        .unwrap();
    assert_eq!(page.total_size, 7);
    let log = fake.log.lock().unwrap();
    assert!(log[1].0.ends_with("&limit=0"), "{}", log[1].0);
}

#[test]
fn a_library_page_maps_its_section_key_back_to_the_view() {
    let (c, fake) = signed_in(vec![
        ("/UserViews", 200, VIEWS),
        ("/Items?", 200, r#"{"Items":[{"Id":"s1","Name":"A Show","Type":"Series"}],"TotalRecordCount":80,"StartIndex":40}"#),
    ]);
    c.libraries().unwrap();
    let page = c
        .library_items(
            2,
            &LibraryPage {
                item_types: "Series",
                sort_by: SortBy::Title,
                descending: false,
                unwatched_only: true,
                starts_with: None,
                genre_id: None,
                start: 40,
                size: 40,
            },
        )
        .unwrap();
    assert_eq!((page.total_size, page.offset, page.metadata[0].library_section_id), (80, 40, 2));
    let log = fake.log.lock().unwrap();
    assert!(log[1].0.contains("parentId=libt&includeItemTypes=Series"), "{}", log[1].0);
    assert!(log[1].0.contains("filters=IsUnplayed"), "{}", log[1].0);
    drop(log); // an unknown key lists the libraries again, through the same fake
    assert!(c.library_items(9, &LibraryPage {
        item_types: "Movie", sort_by: SortBy::Title, descending: false, unwatched_only: false,
        starts_with: None, genre_id: None, start: 0, size: 10 }).is_none(), "a key never issued has no view");
}

#[test]
fn a_librarys_genres_are_listed_for_the_type_its_grid_shows() {
    let (c, fake) = signed_in(vec![
        ("/UserViews", 200, VIEWS),
        ("/Genres?", 200, r#"{"Items":[{"Id":"g1","Name":"Animation","Type":"Genre"},{"Id":"g2","Name":"Comedy","Type":"Genre"}],"TotalRecordCount":2}"#),
    ]);
    c.libraries().unwrap();
    let genres = c.library_genres(2, "Series").unwrap();
    let rows: Vec<_> = genres.directory.iter().map(|d| (d.key.as_str(), d.title.as_str())).collect();
    assert_eq!(rows, [("g1", "Animation"), ("g2", "Comedy")]);
    let log = fake.log.lock().unwrap();
    assert!(log[1].0.contains("/Genres?userId=u1&parentId=libt&includeItemTypes=Series"), "{}", log[1].0);
    drop(log); // an unknown key lists the libraries again, through the same fake
    assert!(c.library_genres(9, "Movie").is_none(), "a key never issued has no view");
}

#[test]
fn a_chosen_genre_narrows_the_library_page() {
    let (c, fake) = signed_in(vec![
        ("/UserViews", 200, VIEWS),
        ("/Items?", 200, r#"{"Items":[{"Id":"m1","Name":"A Film","Type":"Movie"}],"TotalRecordCount":1}"#),
    ]);
    c.libraries().unwrap();
    c.library_items(1, &LibraryPage {
        item_types: "Movie", sort_by: SortBy::Title, descending: false, unwatched_only: false,
        starts_with: None, genre_id: Some("g 1"), start: 0, size: 40 }).unwrap();
    let log = fake.log.lock().unwrap();
    assert!(log[1].0.contains("&genreIds=g%201"), "{}", log[1].0);
}

#[test]
fn detail_carries_skip_markers_when_the_server_has_them() {
    let (c, _) = signed_in(vec![
        ("/Items/e1?", 200, r#"{"Id":"e1","Name":"Pilot","Type":"Episode","RunTimeTicks":27000000000}"#),
        ("/MediaSegments/e1", 200, r#"{"Items":[{"Type":"Intro","StartTicks":0,"EndTicks":600000000}]}"#),
    ]);
    let m = c.metadata("e1").unwrap();
    assert_eq!((m.marker.len(), m.marker[0].kind.as_str()), (1, "intro"));

    let (c, _) = signed_in(vec![
        ("/Items/e1?", 200, r#"{"Id":"e1","Name":"Pilot","Type":"Episode"}"#),
        ("/MediaSegments/e1", 404, ""),
    ]);
    assert!(c.metadata("e1").unwrap().marker.is_empty(), "an older server just has no markers");
}

/// A show's page plays the episode the viewer is in the middle of, or else the next one: the
/// detail carries it as Plex's `OnDeck`, which Play and the opening season tab read.
#[test]
fn a_show_carries_the_episode_to_continue_as_on_deck() {
    let (c, fake) = signed_in(vec![
        ("/Items/sh1?", 200, r#"{"Id":"sh1","Name":"Show","Type":"Series"}"#),
        ("/UserItems/Resume", 200, r#"{"Items":[{"Id":"e7","Name":"Half Way","Type":"Episode","SeriesId":"sh1","ParentIndexNumber":2,"IndexNumber":3,"UserData":{"PlaybackPositionTicks":6000000000}}]}"#),
    ]);
    let m = c.metadata("sh1").unwrap();
    let deck = m.on_deck.as_ref().and_then(|d| d.metadata.as_deref()).expect("an on-deck episode");
    assert_eq!((deck.rating_key.as_str(), deck.parent_index, deck.index, deck.view_offset), ("e7", Some(2), 3, 600_000));
    assert!(fake.log.lock().unwrap().iter().any(|(p, ..)| p.starts_with("/UserItems/Resume") && p.contains("parentId=sh1")));

    // Nothing half-watched: the next episode after the last one watched.
    let (c, fake) = signed_in(vec![
        ("/Items/sh1?", 200, r#"{"Id":"sh1","Name":"Show","Type":"Series"}"#),
        ("/UserItems/Resume", 200, r#"{"Items":[]}"#),
        ("/Shows/NextUp", 200, r#"{"Items":[{"Id":"e8","Name":"Next","Type":"Episode","SeriesId":"sh1","ParentIndexNumber":2,"IndexNumber":4}]}"#),
    ]);
    let m = c.metadata("sh1").unwrap();
    assert_eq!(m.on_deck.as_ref().and_then(|d| d.metadata.as_deref()).map(|e| e.rating_key.as_str()), Some("e8"));
    assert!(fake.log.lock().unwrap().iter().any(|(p, ..)| p.starts_with("/Shows/NextUp") && p.contains("seriesId=sh1")));

    // A movie asks for neither.
    let (c, fake) = signed_in(vec![("/Items/m1?", 200, r#"{"Id":"m1","Name":"Film","Type":"Movie"}"#), ("/MediaSegments/", 404, "")]);
    assert!(c.metadata("m1").unwrap().on_deck.is_none());
    assert!(!fake.log.lock().unwrap().iter().any(|(p, ..)| p.starts_with("/UserItems/Resume") || p.starts_with("/Shows/NextUp")));
}

/// An episode plays in a queue of the rest of its show, the way Plex's continuous play queue
/// does, so the player can offer the next episode and go on to it.
#[test]
fn an_item_scrubs_with_the_narrowest_readable_thumbnails_of_its_played_version() {
    let (c, fake) = signed_in(vec![
        ("/Items/m1?", 200, r#"{"Id":"m1","Name":"Film","Type":"Movie",
            "MediaSources":[{"Id":"src1"},{"Id":"src2"}],
            "Trickplay":{
                "src2":{"320":{"Width":320,"Height":180,"TileWidth":10,"TileHeight":10,"ThumbnailCount":700,"Interval":10000}},
                "src1":{
                    "160":{"Width":160,"Height":90,"TileWidth":10,"TileHeight":10,"ThumbnailCount":600,"Interval":10000},
                    "320":{"Width":320,"Height":134,"TileWidth":10,"TileHeight":10,"ThumbnailCount":600,"Interval":10000},
                    "640":{"Width":640,"Height":268,"TileWidth":5,"TileHeight":5,"ThumbnailCount":600,"Interval":10000}}}}"#),
        ("/Videos/m1/Trickplay/320/3.jpg", 200, "JPEG"),
    ]);
    let t = c.trickplay("m1", 0).unwrap();
    assert_eq!(
        t,
        Trickplay { width: 320, height: 134, tile_cols: 10, tile_rows: 10, count: 600, interval_ms: 10_000 },
        "the first version's, at the narrowest width of at least 240"
    );
    assert!(fake.log.lock().unwrap()[0].0.contains("fields=Trickplay"));
    // 35 minutes in is picture 210: sheet 2, column 0, row 1; the last picture holds past the end.
    assert_eq!(t.index_at(35 * 60_000).map(|i| t.place(i)), Some((2, 0, 1)));
    assert_eq!(t.index_at(i64::MAX / 2), Some(599));
    assert_eq!(c.trickplay_sheet("m1", 0, &t, 3).as_deref(), Some(&b"JPEG"[..]));
    let log = fake.log.lock().unwrap();
    let sheet_req = &log.iter().find(|(p, ..)| p.starts_with("/Videos/")).unwrap().0;
    assert!(sheet_req.starts_with("/Videos/m1/Trickplay/320/3.jpg?ApiKey=tok"), "authorised: {sheet_req}");
    drop(log);

    // No thumbnails, or a layout with a zero in it, is nothing to scrub with.
    let (c, _) = signed_in(vec![("/Items/m2?", 200, r#"{"Id":"m2","Name":"Film","Type":"Movie","MediaSources":[{"Id":"m2"}]}"#)]);
    assert_eq!(c.trickplay("m2", 0), None);
    let (c, _) = signed_in(vec![("/Items/m3?", 200, r#"{"Id":"m3","Name":"Film","Type":"Movie","MediaSources":[{"Id":"m3"}],
        "Trickplay":{"m3":{"320":{"Width":320,"Height":180,"TileWidth":10,"TileHeight":10,"ThumbnailCount":0,"Interval":10000}}}}"#)]);
    assert_eq!(c.trickplay("m3", 0), None);
}

/// The person page's biography comes from the person item; a person the server has never
/// heard of is an empty profile (settled), and a failed request is `None` (retried).
#[test]
fn a_persons_biography_comes_from_the_person_item() {
    let (c, _) = signed_in(vec![
        ("/Items/p1?", 200, r#"{"Id":"p1","Name":"Ana Moreira","Type":"Person","Overview":"Actriz portuguesa.",
            "PremiereDate":"1980-02-03T00:00:00.0000000Z","EndDate":null,"ProductionLocations":["Lisboa, Portugal"]}"#),
        ("/Items/p2?", 404, ""),
        ("/Items/p3?", 500, ""),
    ]);
    let p = c.person_profile("p1").unwrap();
    assert_eq!(
        (p.title.as_str(), p.summary.as_str(), p.born_at.as_str(), p.died_at.as_str(), p.birth_place.as_str()),
        ("Ana Moreira", "Actriz portuguesa.", "1980-02-03", "", "Lisboa, Portugal")
    );
    assert!(c.person_profile("p2").is_some_and(|p| p.title.is_empty() && p.summary.is_empty()));
    assert!(c.person_profile("p3").is_none());
}

/// Each media source is one version, in the server's order, identified by an id taken from the
/// source itself (what upstream's *Play Version* panel asks for). The scrubbing thumbnails of a
/// version other than the first are that version's, and their sheets are asked for by its id.
#[test]
fn every_media_source_is_a_version_and_scrubs_with_its_own_thumbnails() {
    let item = r#"{"Id":"v9","Name":"Film","Type":"Movie",
        "MediaSources":[{"Id":"src1","Name":"1080p","Container":"mkv"},{"Id":"src2","Name":"2160p HDR","Container":"mkv"}],
        "Trickplay":{
            "src1":{"320":{"Width":320,"Height":134,"TileWidth":10,"TileHeight":10,"ThumbnailCount":600,"Interval":10000}},
            "src2":{"320":{"Width":320,"Height":180,"TileWidth":10,"TileHeight":10,"ThumbnailCount":700,"Interval":10000}}}}"#;
    let (c, fake) = signed_in(vec![
        ("/Items/v9?", 200, item),
        ("/Videos/v9/Trickplay/320/0.jpg?mediaSourceId=src2", 200, "JPEG"),
    ]);
    let m = c.metadata("v9").unwrap();
    assert_eq!(m.media.iter().map(|v| v.title.as_str()).collect::<Vec<_>>(), ["1080p", "2160p HDR"]);
    assert_eq!(m.media.iter().map(|v| v.id.0).collect::<Vec<_>>(), [translate::part_id("src1"), translate::part_id("src2")]);
    assert!(m.media.iter().all(|v| v.id.0 != 0), "a version with no id is never offered");
    assert_eq!(m.media_version(Some(m.media[1].id)).map(|(i, _)| i), Some(1));
    assert_eq!(c.trickplay("v9", 0).unwrap().count, 600, "the first version's thumbnails when no part is named");
    assert_eq!(c.trickplay("v9", m.media[0].id.0).unwrap().count, 600, "the first version's");
    let t = c.trickplay("v9", m.media[1].id.0).unwrap();
    assert_eq!(t.count, 700, "the playing version's thumbnails, named by its part");
    assert_eq!(c.trickplay_sheet("v9", m.media[1].id.0, &t, 0).as_deref(), Some(&b"JPEG"[..]));
    assert!(fake.log.lock().unwrap().iter().any(|(p, ..)| p.starts_with("/Videos/v9/Trickplay/320/0.jpg?mediaSourceId=src2&ApiKey=tok")));
}

/// The People tab is fed from the server's `/Persons` search: each hit is a directory tag keyed
/// by the person's id (what the person page opens), with the headshot when the server has one.
#[test]
fn people_are_searched_on_the_persons_endpoint() {
    let (c, fake) = signed_in(vec![(
        "/Persons?",
        200,
        r#"{"Items":[{"Id":"F137A2DD-21BB-C1BB-5C0F-0000000000AA","Name":"Ana Moreira","Type":"Person","ImageTags":{"Primary":"t9"}},
            {"Id":"p2","Name":"Ana Rita","Type":"Person"}],"TotalRecordCount":7,"StartIndex":2}"#,
    )]);
    let mc = c.search_people("ana", 2, 2).unwrap();
    let tags: Vec<_> = mc.search_result.iter().filter_map(|r| r.directory.as_ref()).map(|t| (t.tag.as_str(), t.tag_key.as_str())).collect();
    assert_eq!(tags, [("Ana Moreira", "f137a2dd21bbc1bb5c0f0000000000aa"), ("Ana Rita", "p2")]);
    assert!(mc.search_result.iter().all(|r| r.metadata.is_none()));
    let first = mc.search_result[0].directory.as_ref().unwrap();
    assert!(first.thumb.contains("/Images/Primary") && first.thumb.contains("tag=t9"), "{}", first.thumb);
    assert!(mc.search_result[1].directory.as_ref().unwrap().thumb.is_empty());
    assert_eq!((mc.total_size, mc.offset), (7, 2));
    let path = &fake.log.lock().unwrap()[0].0;
    for part in ["searchTerm=ana", "startIndex=2", "limit=2", "userId=u1"] {
        assert!(path.contains(part), "{part} in {path}");
    }
}

/// The People shelf of a search is the `actor` hub, whose rows are directory tags.
#[test]
fn the_people_shelf_is_an_actor_hub_of_directory_tags() {
    let (c, _) = signed_in(vec![(
        "/Persons?",
        200,
        r#"{"Items":[{"Id":"p1","Name":"Ana Moreira","Type":"Person"}],"TotalRecordCount":1}"#,
    )]);
    let hub = c.people_hub("ana", 10).unwrap();
    assert_eq!((hub.hub_identifier.as_str(), hub.kind.as_str()), ("actor", "actor"));
    assert_eq!(hub.directory.iter().map(|t| t.tag_key.as_str()).collect::<Vec<_>>(), ["p1"]);
    assert!(hub.metadata.is_empty());
}

#[test]
fn a_persons_films_and_shows_come_from_every_library() {
    let (c, fake) = signed_in(vec![(
        "/Items?userId=u1",
        200,
        r#"{"Items":[{"Id":"m1","Name":"Film","Type":"Movie"},{"Id":"s1","Name":"Show","Type":"Series"}],"TotalRecordCount":2}"#,
    )]);
    let mc = c.person_media("p1").unwrap();
    let rows: Vec<_> = mc.metadata.iter().map(|m| (m.rating_key.as_str(), m.kind.as_str())).collect();
    assert_eq!(rows, [("m1", "movie"), ("s1", "show")]);
    let path = &fake.log.lock().unwrap()[0].0;
    for part in ["personIds=p1", "includeItemTypes=Movie%2CSeries", "recursive=true", "sortOrder=Descending"] {
        assert!(path.contains(part), "{part} in {path}");
    }
}

/// Starting a playback asks for the item at each step; the item its page read seconds before is
/// used again, until a write about it (here, marking it watched) makes the next step ask anew.
#[test]
fn a_playback_reads_its_item_once_until_a_write_changes_it() {
    let (c, fake) = signed_in(vec![
        ("/Items/e1?", 200, r#"{"Id":"e1","Name":"Pilot","Type":"Episode","SeriesId":"sh1","ParentIndexNumber":1,"IndexNumber":1}"#),
        ("/Shows/sh1/Episodes", 200, r#"{"Items":[{"Id":"e1","Name":"Pilot","Type":"Episode","SeriesId":"sh1"}]}"#),
        ("/UserPlayedItems/e1", 200, ""),
    ]);
    let reads = || fake.log.lock().unwrap().iter().filter(|(p, ..)| p.starts_with("/Items/e1?")).count();
    assert_eq!(c.metadata("e1").unwrap().title, "Pilot");
    assert_eq!(c.metadata_recent("e1").unwrap().title, "Pilot");
    assert_eq!(c.play_queue("e1", true).unwrap().metadata.len(), 1);
    assert_eq!(reads(), 1, "the page's read serves the playback's steps");
    assert!(fake.log.lock().unwrap().iter().any(|(p, ..)| p.starts_with("/Shows/sh1/Episodes")), "its show from the item");
    assert!(c.metadata_recent("e2").is_none(), "another item is never served from it");
    assert!(c.mark_played("e1"));
    assert!(c.metadata_recent("e1").is_some());
    assert_eq!(reads(), 2, "a write makes the next step ask again");
}

#[test]
fn an_episode_plays_in_a_queue_of_the_rest_of_its_show() {
    let (c, fake) = signed_in(vec![
        ("/Items/e1?", 200, r#"{"Id":"e1","Name":"Pilot","Type":"Episode","SeriesId":"sh1","ParentIndexNumber":1,"IndexNumber":1}"#),
        ("/Shows/sh1/Episodes", 200, r#"{"Items":[
            {"Id":"e1","Name":"Pilot","Type":"Episode","SeriesId":"sh1","ParentIndexNumber":1,"IndexNumber":1},
            {"Id":"e2","Name":"Second","Type":"Episode","SeriesId":"sh1","SeriesName":"Show","ParentIndexNumber":1,"IndexNumber":2,"RunTimeTicks":27000000000}]}"#),
    ]);
    let q = c.play_queue("e1", true).unwrap();
    assert_eq!(q.metadata.len(), 2);
    assert_eq!((q.play_queue_total_count, q.play_queue_selected_item_offset), (2, 0));
    assert_eq!(q.play_queue_selected_item_id, q.metadata[0].play_queue_item_id);
    assert_ne!(q.metadata[0].play_queue_item_id, q.metadata[1].play_queue_item_id);
    assert_eq!((q.metadata[1].rating_key.as_str(), q.metadata[1].kind.as_str(), q.metadata[1].index), ("e2", "episode", 2));
    assert!(q.play_queue_id > 0);
    assert!(fake.log.lock().unwrap().iter().any(|(p, ..)| p.starts_with("/Shows/sh1/Episodes") && p.contains("startItemId=e1")));

    // A movie, or a queue that is not continuous, is the item alone.
    let (c, _) = signed_in(vec![("/Items/m1?", 200, r#"{"Id":"m1","Name":"Film","Type":"Movie"}"#)]);
    let q = c.play_queue("m1", true).unwrap();
    assert_eq!((q.metadata.len(), q.play_queue_total_count), (1, 1));
    let (c, fake) = signed_in(vec![("/Items/e1?", 200, r#"{"Id":"e1","Name":"Pilot","Type":"Episode","SeriesId":"sh1"}"#)]);
    assert_eq!(c.play_queue("e1", false).unwrap().metadata.len(), 1);
    assert!(!fake.log.lock().unwrap().iter().any(|(p, ..)| p.starts_with("/Shows/")));
}

#[test]
fn season_episodes_are_listed_under_the_show() {
    let (c, fake) = signed_in(vec![("/Shows/", 200, r#"{"Items":[{"Id":"e1","Name":"Pilot","Type":"Episode"}]}"#)]);
    let season = Metadata { kind: "season".into(), rating_key: "se1".into(), parent_rating_key: "sh1".into(), ..Default::default() };
    assert_eq!(c.children(&season, 0, 50).unwrap().metadata.len(), 1);
    assert!(fake.log.lock().unwrap()[0].0.starts_with("/Shows/sh1/Episodes?userId=u1&seasonId=se1"));
}

/// Sign-in keeps the user's avatar tag, which the account chip shows (`Backend::signed_in_user`).
#[test]
fn sign_in_keeps_the_users_avatar() {
    let (mut c, _) = client(vec![(
        "/Users/AuthenticateByName",
        200,
        r#"{"User":{"Id":"5F0A-11","Name":"alice","PrimaryImageTag":"abc"},"AccessToken":"tok","ServerId":"SRV"}"#,
    )]);
    let s = c.sign_in("alice", "pw").expect("signed in");
    assert_eq!((s.user_name.as_str(), s.avatar_tag.as_deref()), ("alice", Some("abc")));
    assert_eq!(paths::user_image(&s.user_id, "abc"), "/Users/5f0a11/Images/Primary?quality=90&tag=abc");
    // No avatar set: no tag, and the chip shows the initial.
    let (mut c, _) = client(vec![("/Users/AuthenticateByName", 200, AUTH)]);
    assert_eq!(c.sign_in("alice", "pw").unwrap().avatar_tag, None);
}

/// The server's verdict: a POST carrying the device profile, and its stream re-issued under this
/// client's session and token, from the resume point.
#[test]
fn playback_info_asks_with_the_profile_and_its_stream_is_ours_to_stop() {
    let (c, fake) = signed_in(vec![(
        "/Items/e1/PlaybackInfo",
        200,
        r#"{"MediaSources":[{"Id":"ms1","SupportsDirectPlay":false,
            "TranscodingUrl":"/videos/e1/stream.mkv?DeviceId=dev1&PlaySessionId=srv&api_key=old&TranscodeReasons=SubtitleCodecNotSupported"}]}"#,
    )]);
    let profile = serde_json::json!({"Name": "test"});
    let v = c.playback_info("e1", Some("ms1"), Some(1), Some(3), &profile).unwrap();
    assert!(!v.direct_play && v.video_encode && !v.audio_encode);
    {
        let log = fake.log.lock().unwrap();
        let (path, verb, body, _) = &log[0];
        assert_eq!(*verb, Verb::Post);
        assert!(path.starts_with("/Items/e1/PlaybackInfo?userId=u1&isPlayback=true"));
        assert!(path.contains("&mediaSourceId=ms1&audioStreamIndex=1&subtitleStreamIndex=3"));
        assert_eq!(serde_json::from_str::<serde_json::Value>(body.as_deref().unwrap()).unwrap()["DeviceProfile"]["Name"], "test");
    }
    let url = c.server_transcode_url(v.url.as_deref().unwrap(), "ours", 6_000_000_000);
    assert!(url.starts_with("http://jf:8096/videos/e1/stream.mkv?DeviceId=dev1"));
    assert!(url.contains("PlaySessionId=ours") && url.contains("StartTimeTicks=6000000000") && url.ends_with("ApiKey=tok"));
    assert!(!url.contains("srv") && !url.contains("api_key=old"));
    assert!(c.is_transcoding("ours"), "stopped with the rest of this client's transcodes");
}

/// The user's policy rides with the verdict: without video transcoding the server copies the
/// picture, so a video reason is not an encode.
#[test]
fn playback_info_reads_whether_the_user_may_transcode() {
    let (c, _) = signed_in(vec![
        (
            "/Items/e1/PlaybackInfo",
            200,
            r#"{"MediaSources":[{"Id":"ms1","SupportsDirectPlay":false,
                "TranscodingUrl":"/videos/e1/stream.mkv?TranscodeReasons=VideoCodecNotSupported,AudioCodecNotSupported"}]}"#,
        ),
        ("/Users/u1", 200, r#"{"Id":"u1","Policy":{"EnableVideoPlaybackTranscoding":false,"EnableAudioPlaybackTranscoding":true}}"#),
    ]);
    let v = c.playback_info("e1", Some("ms1"), None, None, &serde_json::json!({})).unwrap();
    assert!(!v.direct_play && !v.video_encode && v.audio_encode);
}

/// The favourite write: POST to add, DELETE to remove, on the current route, and the older
/// `/Users/{userId}/FavoriteItems/{itemId}` when a server answers 404 to it.
#[test]
fn favourites_are_written_with_a_fallback_to_the_older_route() {
    let (c, fake) = signed_in(vec![("/UserFavoriteItems/", 200, "{}")]);
    assert!(c.set_favorite("m1", true) && c.set_favorite("m1", false));
    {
        let log = fake.log.lock().unwrap();
        assert_eq!((log[0].0.as_str(), log[0].1), ("/UserFavoriteItems/m1?userId=u1", Verb::Post));
        assert_eq!((log[1].0.as_str(), log[1].1), ("/UserFavoriteItems/m1?userId=u1", Verb::Delete));
    }
    let (c, fake) = signed_in(vec![("/UserFavoriteItems/", 404, ""), ("/Users/u1/FavoriteItems/", 200, "{}")]);
    assert!(c.set_favorite("m2", true));
    assert_eq!(fake.log.lock().unwrap()[1].0, "/Users/u1/FavoriteItems/m2");
    let (c, _) = signed_in(vec![("/UserFavoriteItems/", 500, "")]);
    assert!(!c.set_favorite("m3", true), "a refusal is reported, so the mark flips back");
}

/// Reading an item records its favourite mark for the menu and the detail page.
#[test]
fn an_items_favourite_mark_is_recorded_as_it_is_read() {
    let (c, _) = signed_in(vec![("/Items/fav9", 200, r#"{"Id":"fav9","Name":"Film","Type":"Movie","UserData":{"IsFavorite":true}}"#)]);
    c.metadata("fav9").unwrap();
    assert!(crate::jellyfin::favorites::state("fav9"));
}

#[test]
fn watched_state_writes_use_the_right_verbs() {
    let (c, fake) = signed_in(vec![("/UserPlayedItems/", 200, "{}"), ("/UserItems/", 200, "{}")]);
    assert!(c.mark_played("m1") && c.mark_unplayed("m1"));
    let log = fake.log.lock().unwrap();
    let verbs: Vec<_> = log.iter().map(|(p, v, b, _)| (p.as_str(), *v, b.clone())).collect();
    assert_eq!(verbs[0], ("/UserPlayedItems/m1?userId=u1", Verb::Post, None));
    assert_eq!(verbs[1], ("/UserPlayedItems/m1?userId=u1", Verb::Delete, None));
    assert_eq!(verbs.len(), 2);
}

/// A movie leaves Continue Watching with one write that only clears the resume point.
#[test]
fn a_movie_leaves_continue_watching_with_one_write() {
    let (c, fake) = signed_in(vec![("/UserItems/", 200, "{}")]);
    assert!(c.leave_continue_watching("m2", false));
    let log = fake.log.lock().unwrap();
    assert_eq!(log.len(), 1);
    assert_eq!(log[0].0, "/UserItems/m2/UserData?userId=u1");
    assert_eq!(log[0].1, Verb::Post);
    assert_eq!(log[0].2.as_deref(), Some(r#"{"PlaybackPositionTicks":0}"#));
}

/// An episode is also recorded as dismissed, and Next Up then leaves it out.
#[test]
fn a_dismissed_episode_stays_out_of_next_up() {
    let prefs = r#"{"Id":"finnative","CustomPrefs":{"dismissedNextUp":"e0"}}"#;
    let (c, fake) = signed_in(vec![
        ("/UserItems/", 200, "{}"),
        ("/DisplayPreferences/finnative", 200, prefs),
        ("/Shows/NextUp", 200, r#"{"Items":[{"Id":"e0","Name":"Gone","Type":"Episode"},{"Id":"e9","Name":"Kept","Type":"Episode"}]}"#),
    ]);
    assert!(c.leave_continue_watching("e1", true));
    {
        let log = fake.log.lock().unwrap();
        let write = log.iter().find(|(p, v, ..)| p.starts_with("/DisplayPreferences/") && *v == Verb::Post).unwrap();
        assert!(write.2.as_deref().unwrap().contains(r#""dismissedNextUp":"e0,e1""#), "{:?}", write.2);
    }
    let names: Vec<_> = c.next_up(0, 10).unwrap().metadata.iter().map(|m| m.title.clone()).collect();
    assert_eq!(names, ["Kept"]);
}

/// The dismissed list is read once and then reused: Home's deck reads do not ask for it again,
/// and this client's own dismissal is seen at once without another read.
#[test]
fn the_dismissed_list_is_read_once_and_kept_by_this_clients_write() {
    let prefs = r#"{"Id":"finnative","CustomPrefs":{"dismissedNextUp":"e0"}}"#;
    let (c, fake) = signed_in(vec![
        ("/UserItems/", 200, "{}"),
        ("/DisplayPreferences/finnative", 200, prefs),
        ("/Shows/NextUp", 200, r#"{"Items":[{"Id":"e0","Name":"Gone","Type":"Episode"},
            {"Id":"e1","Name":"Dismissed here","Type":"Episode"},{"Id":"e9","Name":"Kept","Type":"Episode"}]}"#),
    ]);
    let prefs_reads = || fake.log.lock().unwrap().iter()
        .filter(|(p, v, ..)| p.starts_with("/DisplayPreferences/") && *v == Verb::Get).count();
    let titles = |c: &JfClient| c.next_up(0, 10).unwrap().metadata.iter().map(|m| m.title.clone()).collect::<Vec<_>>();
    assert_eq!(titles(&c), ["Dismissed here", "Kept"]);
    assert_eq!(titles(&c), ["Dismissed here", "Kept"]);
    assert_eq!(prefs_reads(), 1, "read once for both deck reads");
    assert!(c.leave_continue_watching("e1", true));
    assert_eq!(prefs_reads(), 2, "the write starts from the server's record");
    assert_eq!(titles(&c), ["Kept"], "this client's write is seen at once");
    assert_eq!(prefs_reads(), 2, "without another read");
}

/// Next Up is paged after the dismissed episodes are left out: the server's own paging counts
/// them, so a second page and the total are this app's, not the server's.
#[test]
fn next_up_pages_after_the_dismissed_episodes_are_left_out() {
    let prefs = r#"{"Id":"finnative","CustomPrefs":{"dismissedNextUp":"e0"}}"#;
    let (c, fake) = signed_in(vec![
        ("/DisplayPreferences/finnative", 200, prefs),
        ("/Shows/NextUp", 200, r#"{"TotalRecordCount":3,"Items":[{"Id":"e0","Name":"Gone","Type":"Episode"},
            {"Id":"e1","Name":"First","Type":"Episode"},{"Id":"e2","Name":"Second","Type":"Episode"}]}"#),
    ]);
    let page = c.next_up(1, 1).unwrap();
    let names: Vec<_> = page.metadata.iter().map(|m| m.title.clone()).collect();
    assert_eq!(names, ["Second"]);
    assert_eq!((page.offset, page.size, page.total_size), (1, 1, 2));
    let log = fake.log.lock().unwrap();
    let read = log.iter().find(|(p, ..)| p.starts_with("/Shows/NextUp")).unwrap();
    assert!(read.0.contains("startIndex=0&limit=52&"), "read from the top, past the window: {}", read.0);
}

#[test]
fn subtitles_are_searched_in_the_servers_language_codes_and_downloaded_by_handle() {
    let (c, fake) = signed_in(vec![
        ("/Items/m1/RemoteSearch/Subtitles/por", 200, r#"[
            {"Id":"opensubtitles_1","ProviderName":"Open Subtitles","Name":"Film.2019.WEB","Format":"srt",
             "ThreeLetterISOLanguageName":"por","DownloadCount":900,"IsHashMatch":false},
            {"Id":"opensubtitles_2","ProviderName":"Open Subtitles","Name":"Film.2019.BluRay","Format":"SRT",
             "ThreeLetterISOLanguageName":"por","DownloadCount":12,"IsHashMatch":true,"HearingImpaired":true},
            {"Id":null,"Name":"no handle"}]"#),
        ("/Items/m1/RemoteSearch/Subtitles/opensubtitles_2", 204, ""),
    ]);
    let found = c.subtitle_candidates("m1", "pt").unwrap();
    let rows: Vec<_> = found.stream.iter().map(|s| (s.key.as_str(), s.stream_type, s.format.as_str(), s.hearing_impaired)).collect();
    assert_eq!(rows, [("opensubtitles_1", 3, "srt", 0), ("opensubtitles_2", 3, "srt", 1)], "a row without a handle is dropped");
    assert_eq!((found.stream[0].provider_title.as_str(), found.stream[0].language_code.as_str()), ("Open Subtitles", "por"));
    assert!(found.stream[1].score > found.stream[0].score, "a match on the file outranks downloads");
    assert!(c.download_subtitle("m1", "opensubtitles_2"));
    let log = fake.log.lock().unwrap();
    assert!(log[0].0.starts_with("/Items/m1/RemoteSearch/Subtitles/por?"), "{}", log[0].0);
    assert_eq!((log[1].0.as_str(), log[1].1), ("/Items/m1/RemoteSearch/Subtitles/opensubtitles_2", Verb::Post));
}

#[test]
fn playback_reports_start_then_progress_then_stop() {
    let (c, fake) = signed_in(vec![("/Sessions/Playing", 204, "")]);
    let r = |state, position_ms| PlaybackReport {
        item_id: "m1",
        play_session_id: "ps1",
        state,
        position_ms,
        audio_index: 1,
        subtitle_index: 0,
    };
    assert!(c.report(&r(PlayState::Playing, 0)));
    assert!(c.report(&r(PlayState::Paused, 61_000)));
    assert!(c.report(&r(PlayState::Stopped, 62_000)));
    assert!(c.report(&r(PlayState::Playing, 0)), "a new play of the same session starts again");
    let log = fake.log.lock().unwrap();
    let paths: Vec<_> = log.iter().map(|(p, ..)| p.as_str()).collect();
    assert_eq!(paths, ["/Sessions/Playing", "/Sessions/Playing/Progress", "/Sessions/Playing/Stopped", "/Sessions/Playing"]);
    let body: serde_json::Value = serde_json::from_str(log[1].2.as_deref().unwrap()).unwrap();
    assert_eq!(body["PositionTicks"], 610_000_000);
    assert_eq!(body["IsPaused"], true);
    assert_eq!(body["AudioStreamIndex"], 1);
    assert!(body.get("SubtitleStreamIndex").is_none());
}

#[test]
fn subtitle_bytes_carry_the_token_and_report_why_not() {
    let (c, fake) = signed_in(vec![("/Videos/m1/ms/Subtitles/3/0/Stream.srt", 200, "1\n00:00:01,000 --> 00:00:02,000\nHi\n")]);
    assert!(c.get_bytes("/Videos/m1/ms/Subtitles/3/0/Stream.srt").unwrap().starts_with(b"1"));
    assert_eq!(fake.log.lock().unwrap()[0].0, "/Videos/m1/ms/Subtitles/3/0/Stream.srt?ApiKey=tok");
    let (c, _) = signed_in(vec![("/Videos/", 404, "")]);
    assert_eq!(c.get_bytes("/Videos/x").unwrap_err(), "HTTP 404 (0 bytes)");
}

#[test]
fn library_kinds_are_remembered_from_the_listing() {
    let (c, fake) = signed_in(vec![("/UserViews", 200, VIEWS)]);
    // A kind asked for before any listing lists the libraries itself, once.
    assert_eq!((c.library_kind(1).as_deref(), c.library_kind(2).as_deref()), (Some("movie"), Some("show")));
    assert_eq!(fake.log.lock().unwrap().iter().filter(|(p, ..)| p.starts_with("/UserViews")).count(), 1);
}

/// A section call made before any listing (a resumed sign-in lists nothing up front) lists the
/// libraries first and then asks for that library.
#[test]
fn a_section_call_before_any_listing_lists_the_libraries_first() {
    let (c, fake) = signed_in(vec![("/UserViews", 200, VIEWS), ("/Genres", 200, r#"{"Items":[]}"#)]);
    assert!(c.library_genres(1, "Movie").is_some());
    let log = fake.log.lock().unwrap();
    let paths: Vec<&str> = log.iter().map(|(p, ..)| p.split('?').next().unwrap_or_default()).collect();
    assert_eq!(paths, ["/UserViews", "/Genres"]);
}

#[test]
fn urls_for_the_player_and_the_image_loader() {
    let (c, _) = signed_in(vec![]);
    assert_eq!(
        c.direct_play_url("/Videos/m1/stream?static=true&mediaSourceId=ms1", Some("ps1")),
        "http://jf:8096/Videos/m1/stream?static=true&mediaSourceId=ms1&playSessionId=ps1&ApiKey=tok"
    );
    assert_eq!(
        c.image_path("/Items/m1/Images/Primary?quality=90&tag=t", 300, 450),
        "/Items/m1/Images/Primary?quality=90&tag=t&fillWidth=300&fillHeight=450"
    );
}

const USER: &str = r#"{"Name":"alice","Id":"u1","Policy":{"IsAdministrator":true},
    "Configuration":{"AudioLanguagePreference":"por","PlayDefaultAudioTrack":false,
    "SubtitleLanguagePreference":"eng","SubtitleMode":"Smart","DisplayMissingEpisodes":true,
    "OrderedViews":["lib-m","lib-t"],"CastReceiverId":"F007D354","SomethingNew":{"x":1}}}"#;

#[test]
fn preferences_read_the_users_configuration() {
    let (c, fake) = signed_in(vec![("/Users/u1", 200, USER)]);
    let cfg = c.language_preferences().unwrap();
    assert_eq!(
        (cfg.audio_language_preference.as_str(), cfg.play_default_audio_track),
        ("por", Some(false))
    );
    assert_eq!((cfg.subtitle_language_preference.as_str(), cfg.subtitle_mode.as_str()), ("eng", "Smart"));
    assert_eq!(fake.log.lock().unwrap()[0].0, "/Users/u1");

    let (c, _) = signed_in(vec![("/Users/u1", 200, r#"{"Name":"alice","Id":"u1","Configuration":null}"#)]);
    assert_eq!(c.language_preferences(), Err(PreferenceError::InvalidResponse));
    let (c, _) = signed_in(vec![("/Users/u1", 401, "")]);
    assert_eq!(c.language_preferences(), Err(PreferenceError::Refused));
    let (c, _) = signed_in(vec![]);
    assert_eq!(c.language_preferences(), Err(PreferenceError::Unavailable));
    let (c, fake) = client(vec![("/Users/", 200, USER)]);
    assert_eq!(c.language_preferences(), Err(PreferenceError::Unavailable), "no user signed in");
    assert!(fake.log.lock().unwrap().is_empty());
}

#[test]
fn a_save_posts_the_whole_configuration_back_with_only_its_keys_changed() {
    let (c, fake) = signed_in(vec![("/Users/Configuration", 204, ""), ("/Users/u1", 200, USER)]);
    let update = UserLanguageUpdate {
        subtitle_language_preference: Some("por".into()),
        subtitle_mode: Some("Always"),
        ..Default::default()
    };
    let saved = c.save_language_preferences(&update).unwrap();
    assert_eq!((saved.subtitle_language_preference.as_str(), saved.subtitle_mode.as_str()), ("por", "Always"));
    assert_eq!(saved.audio_language_preference, "por");

    let log = fake.log.lock().unwrap();
    let (path, verb, body, _) = &log[1];
    assert_eq!((path.as_str(), *verb), ("/Users/Configuration?userId=u1", Verb::Post));
    let posted: serde_json::Value = serde_json::from_str(body.as_deref().unwrap()).unwrap();
    let mut expected: serde_json::Value = serde_json::from_str(USER).unwrap();
    let expected = expected.get_mut("Configuration").unwrap().take();
    let mut expected = expected.as_object().unwrap().clone();
    expected.insert("SubtitleLanguagePreference".into(), "por".into());
    expected.insert("SubtitleMode".into(), "Always".into());
    assert_eq!(posted, serde_json::Value::Object(expected), "every other key goes back as it came");
}

#[test]
fn a_refused_save_says_so() {
    let (c, _) = signed_in(vec![("/Users/Configuration", 403, ""), ("/Users/u1", 200, USER)]);
    let update = UserLanguageUpdate { subtitle_mode: Some("None"), ..Default::default() };
    assert_eq!(c.save_language_preferences(&update), Err(PreferenceError::Refused));
}

/// Asking whose a saved token is tells a token the server stopped accepting from a server that did
/// not answer: a profile switch reads the first as "sign in again" and seats the second offline.
#[test]
fn whose_token_it_is_tells_a_refused_token_from_no_answer() {
    let (mut c, _) = client(vec![("/Users/Me", 200, r#"{"Id":"5F0A-11","Name":"alice","ServerId":"SRV"}"#)]);
    let me = c.who_is("tok").expect("the user");
    assert_eq!((me.user_name.as_str(), me.token.as_str()), ("alice", "tok"));
    assert_eq!(c.session().map(|s| s.token.as_str()), Some("tok"));
    for status in [401, 403] {
        let (mut c, _) = client(vec![("/Users/Me", status, "")]);
        assert_eq!(c.who_is("tok"), Err(SignInError::Rejected));
        assert!(c.session().is_none());
    }
    let (mut c, _) = client(vec![("/Users/Me", 500, "")]);
    assert_eq!(c.who_is("tok"), Err(SignInError::Server(500)));
    let (mut c, _) = client(vec![]);
    assert_eq!(c.who_is("tok"), Err(SignInError::Unreachable));
    assert!(c.who_is("tok").is_err());
}

/// A launch asks for Home's shelves, the library list and the saved user's name all at once: one
/// read of the user and one of the libraries answer every one of them.
#[test]
fn a_launch_reads_the_user_and_the_libraries_once() {
    let (c, fake) = signed_in(vec![
        ("/UserViews", 200, VIEWS),
        ("/Users/u1", 200, r#"{"Id":"u1","Name":"t","Configuration":{"OrderedViews":[]},"Policy":{}}"#),
        ("/Items?userId=u1", 200, r#"{"Items":[]}"#),
        ("/Items/Latest", 200, "[]"),
    ]);
    std::thread::scope(|s| {
        s.spawn(|| c.home_hubs());
        s.spawn(|| c.libraries_recent(FRESH));
        s.spawn(|| c.user_value(FRESH));
    });
    assert!(c.user_value(RECENT_CONFIG).is_ok(), "a play's policy reads the same user");
    let log = fake.log.lock().unwrap();
    let count = |prefix: &str| log.iter().filter(|(p, ..)| p.starts_with(prefix)).count();
    assert_eq!((count("/UserViews"), count("/Users/u1")), (1, 1));
}

/// A user this client changed, or the server says changed, is read again.
#[test]
fn a_forgotten_user_is_read_again() {
    let (c, fake) = signed_in(vec![("/Users/u1", 200, r#"{"Id":"u1","Name":"t"}"#)]);
    c.user_value(FRESH).unwrap();
    c.forget_user();
    c.user_value(FRESH).unwrap();
    assert_eq!(fake.log.lock().unwrap().len(), 2);
}

/// Progress keeps the item a playback's later steps read (a seek's transcode, an audio switch);
/// the end of the playback, which moved its resume point, drops it.
#[test]
fn only_a_stop_forgets_the_playing_item() {
    let (c, fake) = signed_in(vec![
        ("/Items/m1", 200, r#"{"Id":"m1","Name":"Film","Type":"Movie"}"#),
        ("/MediaSegments/m1", 200, r#"{"Items":[]}"#),
        ("/Sessions/Playing", 204, ""),
    ]);
    let r = |state| PlaybackReport { item_id: "m1", play_session_id: "ps1", state, position_ms: 1_000, audio_index: 0, subtitle_index: 0 };
    c.metadata("m1").unwrap();
    c.report(&r(PlayState::Playing));
    c.report(&r(PlayState::Playing));
    c.metadata_recent("m1").unwrap();
    c.report(&r(PlayState::Stopped));
    c.metadata_recent("m1").unwrap();
    let log = fake.log.lock().unwrap();
    assert_eq!(log.iter().filter(|(p, ..)| p.starts_with("/Items/m1")).count(), 2, "read at the page and after the stop");
}

/// A page lists its children right after reading the item: the item it read says what it is.
#[test]
fn children_of_a_page_just_read_ask_once() {
    let (c, fake) = signed_in(vec![
        ("/Items/s1", 200, r#"{"Id":"s1","Name":"Show","Type":"Series"}"#),
        ("/MediaSegments/s1", 200, r#"{"Items":[]}"#),
        ("/Shows/s1/Seasons", 200, r#"{"Items":[{"Id":"se1","Name":"Season 1","Type":"Season"}]}"#),
        ("/Shows/s1/NextUp", 200, r#"{"Items":[]}"#),
        ("/Shows/NextUp", 200, r#"{"Items":[]}"#),
        ("/UserItems/Resume", 200, r#"{"Items":[]}"#),
    ]);
    c.metadata("s1").unwrap();
    fake.log.lock().unwrap().clear();
    assert_eq!(c.children_of("s1", 0, 50).unwrap().metadata.len(), 1);
    let log = fake.log.lock().unwrap();
    let paths: Vec<_> = log.iter().map(|(p, ..)| p.split('?').next().unwrap_or("")).collect();
    assert_eq!(paths, ["/Shows/s1/Seasons"]);
}

/// The scrubbing thumbnails of the item a playback just read come from that read.
#[test]
fn a_played_items_thumbnails_come_from_its_page() {
    let (c, fake) = signed_in(vec![
        ("/Items/m1?", 200, r#"{"Id":"m1","Name":"Film","Type":"Movie","MediaSources":[{"Id":"src1"}],
            "Trickplay":{"src1":{"320":{"Width":320,"Height":134,"TileWidth":10,"TileHeight":10,"ThumbnailCount":600,"Interval":10000}}}}"#),
        ("/MediaSegments/m1", 200, r#"{"Items":[]}"#),
    ]);
    c.metadata("m1").unwrap();
    assert_eq!(c.trickplay("m1", 0).map(|t| t.width), Some(320));
    let log = fake.log.lock().unwrap();
    assert_eq!(log.iter().filter(|(p, ..)| p.starts_with("/Items/m1")).count(), 1);
    assert!(log[0].0.contains("Trickplay") || log[1].0.contains("Trickplay"), "the page asks for them");
}
