//! The Seerr client against canned transports: the wire it sends (paths, encoding, the cookie, the
//! JSON bodies), the shapes it reads (lists, a show's seasons, the request list), the status map,
//! the address rules and the dev-build write failsafe. Host tests never reach curl.
use super::*;

const BASE: &str = "http://127.0.0.1:5055";

fn client() -> SeerrClient {
    SeerrClient::for_test(BASE)
}

fn session() -> SeerrSession {
    SeerrSession::for_test("connect.sid=s%3Asynthetic")
}

fn ok(body: &str) -> Result<SeerrResponse, SeerrError> {
    Ok(SeerrResponse { status: 200, body: body.as_bytes().to_vec(), cookies: Vec::new() })
}

fn status(code: u16) -> Result<SeerrResponse, SeerrError> {
    Ok(SeerrResponse { status: code, body: b"{}".to_vec(), cookies: Vec::new() })
}

/// Run `f` against a closure that answers `replies` in order and records every request.
fn run<T>(replies: Vec<Result<SeerrResponse, SeerrError>>, f: impl FnOnce(&SeerrClient, SeerrTransport) -> T)
    -> (T, Vec<SeerrRequest>)
{
    let mut seen = Vec::new();
    let mut queue = replies.into_iter();
    let out = {
        let mut transport = |r: &SeerrRequest| {
            seen.push(r.clone());
            queue.next().expect("more requests than canned replies")
        };
        f(&client(), &mut transport)
    };
    (out, seen)
}

fn body_json(r: &SeerrRequest) -> Value {
    serde_json::from_slice(r.body.as_deref().expect("a body")).expect("JSON")
}

#[test]
fn sign_in_posts_the_plex_token_and_keeps_the_cookies_as_the_session() {
    let reply = Ok(SeerrResponse { status: 200, body: br#"{"id":7,"displayName":"sabc12345","plexUsername":"x"}"#.to_vec(),
        cookies: vec!["connect.sid=s%3Aone".into(), "XSRF-TOKEN=two".into()] });
    let (got, seen) = run(vec![reply], |c, t| c.sign_in("synthetic-plex-token", t));
    let (session, user) = got.expect("signed in");
    assert_eq!((user.id, user.name.as_str()), (7, "sabc12345"));
    assert_eq!(session, SeerrSession::for_test("connect.sid=s%3Aone; XSRF-TOKEN=two"));
    let r = &seen[0];
    assert_eq!((r.method, r.url.as_str(), r.cookie.as_deref()), ("POST", "http://127.0.0.1:5055/api/v1/auth/plex", None));
    assert_eq!(body_json(r), json!({ "authToken": "synthetic-plex-token" }));
}

#[test]
fn a_sign_in_without_a_cookie_or_a_refused_one_is_not_a_session() {
    let (got, _) = run(vec![ok(r#"{"id":7}"#)], |c, t| c.sign_in("t", t));
    assert_eq!(got.unwrap_err(), SeerrError::Unreadable, "no cookie, no session");
    let (got, _) = run(vec![status(403)], |c, t| c.sign_in("t", t));
    assert_eq!(got.unwrap_err(), SeerrError::Forbidden, "a Plex user this server does not let in");
}

#[test]
fn every_later_call_carries_the_session_cookie() {
    let (_, seen) = run(vec![ok(r#"{"id":7}"#)], |c, t| c.me(&session(), t));
    assert_eq!(seen[0].cookie.as_deref(), Some("connect.sid=s%3Asynthetic"));
    assert_eq!(seen[0].url, "http://127.0.0.1:5055/api/v1/auth/me");
}

#[test]
fn search_encodes_the_query_and_keeps_movies_and_shows_only() {
    let body = r#"{"page":1,"totalPages":3,"results":[
        {"id":11,"mediaType":"movie","title":"s00000001","releaseDate":"2019-05-01","posterPath":"/p.jpg",
         "mediaInfo":{"status":5}},
        {"id":12,"mediaType":"tv","name":"s00000002","firstAirDate":"2021-01-01","mediaInfo":{"status":2}},
        {"id":13,"mediaType":"person","name":"s00000003"},
        {"id":14,"mediaType":"movie","title":"s00000004"},
        {"mediaType":"movie","title":"no id"}]}"#;
    let (got, seen) = run(vec![ok(body)], |c, t| c.search(&session(), "a b&c'é", 1, t));
    assert_eq!(seen[0].url, "http://127.0.0.1:5055/api/v1/search?query=a%20b%26c%27%C3%A9&page=1",
        "every reserved byte is percent-encoded: Overseerr refuses a raw quote or space");
    let page = got.expect("a page");
    assert_eq!((page.page, page.pages), (1, 3));
    let rows: Vec<_> = page.items.iter().map(|t| (t.kind, t.tmdb, t.title.as_str(), t.year, t.status)).collect();
    assert_eq!(rows, [
        (Some(MediaKind::Movie), 11, "s00000001", Some(2019), Availability::Available),
        (Some(MediaKind::Tv), 12, "s00000002", Some(2021), Availability::Pending),
        (Some(MediaKind::Movie), 14, "s00000004", None, Availability::Unknown),
    ]);
    assert_eq!(page.items[0].poster.as_deref(), Some("/p.jpg"));
}

#[test]
fn a_discover_row_reads_its_path_and_takes_the_rows_kind_when_they_omit_it() {
    let (got, seen) = run(vec![ok(r#"{"page":2,"totalPages":9,"results":[{"id":21,"name":"s00000021"}]}"#)],
        |c, t| c.discover(&session(), DiscoverRow::PopularTv, 2, t));
    assert_eq!(seen[0].url, "http://127.0.0.1:5055/api/v1/discover/tv?page=2");
    let page = got.unwrap();
    assert_eq!((page.items[0].kind, page.items[0].title.as_str()), (Some(MediaKind::Tv), "s00000021"));
    for (row, path) in [(DiscoverRow::Trending, "/discover/trending"), (DiscoverRow::PopularMovies, "/discover/movies"),
        (DiscoverRow::UpcomingMovies, "/discover/movies/upcoming"), (DiscoverRow::UpcomingTv, "/discover/tv/upcoming")] {
        let (_, seen) = run(vec![ok(r#"{"results":[]}"#)], |c, t| c.discover(&session(), row, 0, t));
        assert_eq!(seen[0].url, format!("{BASE}/api/v1{path}?page=1"), "{row:?}: page 0 asks for page 1");
    }
}

#[test]
fn a_shows_seasons_fold_what_the_server_holds_with_what_is_asked_for() {
    let body = r#"{"id":31,"name":"s00000031","firstAirDate":"2015-02-02","numberOfSeasons":5,
        "seasons":[{"seasonNumber":0,"episodeCount":3},{"seasonNumber":1,"episodeCount":10},
                   {"seasonNumber":2,"episodeCount":10},{"seasonNumber":3,"episodeCount":8},
                   {"seasonNumber":4,"episodeCount":8},{"seasonNumber":5,"episodeCount":6}],
        "mediaInfo":{"status":4,
            "seasons":[{"seasonNumber":1,"status":5},{"seasonNumber":2,"status":4}],
            "requests":[{"status":1,"seasons":[{"seasonNumber":3}]},
                        {"status":2,"seasons":[{"seasonNumber":4},{"seasonNumber":1}]},
                        {"status":3,"seasons":[{"seasonNumber":5}]}]}}"#;
    let (got, seen) = run(vec![ok(body)], |c, t| c.detail(&session(), MediaKind::Tv, 31, t));
    assert_eq!(seen[0].url, "http://127.0.0.1:5055/api/v1/tv/31");
    let detail = got.unwrap();
    assert_eq!(detail.title.status, Availability::PartlyAvailable);
    let seasons: Vec<_> = detail.seasons.iter().map(|s| (s.number, s.status)).collect();
    assert_eq!(seasons, [
        (1, Availability::Available), // held; an approved request for it cannot lower that
        (2, Availability::PartlyAvailable),
        (3, Availability::Pending), // a request waiting for approval
        (4, Availability::Processing), // an approved one
        (5, Availability::Unknown), // a declined request asks for nothing
    ], "specials are left out");
    assert_eq!(detail.requestable_seasons(), [5]);
}

#[test]
fn a_movie_detail_has_no_seasons() {
    let (got, seen) = run(vec![ok(r#"{"id":41,"title":"s00000041","seasons":[{"seasonNumber":1}]}"#)],
        |c, t| c.detail(&session(), MediaKind::Movie, 41, t));
    assert_eq!(seen[0].url, "http://127.0.0.1:5055/api/v1/movie/41");
    let detail = got.unwrap();
    assert!(detail.seasons.is_empty());
    assert_eq!((detail.title.kind, detail.title.status), (Some(MediaKind::Movie), Availability::Unknown));
}

#[test]
fn the_request_list_pages_by_skip_newest_change_first_and_names_no_requester() {
    let body = r#"{"pageInfo":{"pages":4,"page":2},"results":[
        {"id":5,"status":1,"type":"tv","media":{"tmdbId":51,"mediaType":"tv","status":2},
         "seasons":[{"seasonNumber":2},{"seasonNumber":3}]},
        {"id":6,"status":2,"media":{"tmdbId":52,"mediaType":"movie","status":3}},
        {"id":7,"status":1,"media":{"mediaType":"movie"}}]}"#;
    let (got, seen) = run(vec![ok(body)], |c, t| c.requests(&session(), 2, t));
    assert_eq!(seen[0].url, "http://127.0.0.1:5055/api/v1/request?take=20&skip=20&filter=all&sort=modified",
        "the server decides whose requests a user sees");
    let page = got.unwrap();
    assert_eq!((page.page, page.pages), (2, 4));
    let bare = |id, kind, tmdb, status, availability, seasons: Vec<i32>| MediaRequest {
        id, kind, tmdb, status, availability, seasons, requested_by: None, modified_by: None, created: None,
        updated: None, downloads: Vec::new(),
    };
    assert_eq!(page.items, [
        bare(5, MediaKind::Tv, 51, RequestStatus::PendingApproval, Availability::Pending, vec![2, 3]),
        bare(6, MediaKind::Movie, 52, RequestStatus::Approved, Availability::Processing, vec![]),
    ], "a row with no tmdb id is dropped; a row without users, dates or downloads still reads");
}

#[test]
fn a_request_row_reads_who_asked_who_answered_when_and_its_downloads() {
    let body = r#"{"pageInfo":{"pages":1},"results":[
        {"id":9,"status":2,"createdAt":"2026-10-28T09:15:00.000Z","updatedAt":"2026-10-29T10:00:00.000Z",
         "requestedBy":{"id":3,"displayName":"Alex"},"modifiedBy":{"id":1,"plexUsername":"owner"},
         "media":{"tmdbId":90,"mediaType":"tv","status":3,"downloadStatus":[
           {"title":"Show.S02E01","size":1000,"sizeLeft":250,"status":"downloading","timeLeft":"00:12:30",
            "episode":{"seasonNumber":2,"episodeNumber":1}},
           {"title":"Show.S02E02","size":3000,"sizeLeft":3000,"status":"queued","timeLeft":"1.02:00:00"}]},
         "seasons":[{"seasonNumber":2}]},
        {"id":10,"status":4,"media":{"tmdbId":91,"mediaType":"movie","status":1}},
        {"id":11,"status":5,"media":{"tmdbId":92,"mediaType":"movie","status":5}}]}"#;
    let (got, _) = run(vec![ok(body)], |c, t| c.requests(&session(), 1, t));
    let items = got.unwrap().items;
    let first = &items[0];
    assert_eq!((first.requested_by.as_deref(), first.modified_by.as_deref()), (Some("Alex"), Some("owner")));
    assert_eq!((first.created.as_deref(), first.updated.as_deref()),
        (Some("2026-10-28T09:15:00.000Z"), Some("2026-10-29T10:00:00.000Z")));
    assert_eq!(first.downloads, [
        Download { title: "Show.S02E01".into(), size: 1000, size_left: 250, time_left_ms: Some(750_000),
            state: DownloadState::Downloading, episode: Some((2, 1)) },
        Download { title: "Show.S02E02".into(), size: 3000, size_left: 3000, time_left_ms: Some(93_600_000),
            state: DownloadState::Queued, episode: None },
    ]);
    assert_eq!((items[1].status, items[2].status), (RequestStatus::Failed, RequestStatus::Completed));
}

#[test]
fn a_titles_downloads_add_up_to_one_progress() {
    let d = |size, left, ms, state| Download { title: String::new(), size, size_left: left, time_left_ms: ms, state,
        episode: None };
    assert_eq!(progress(&[]), None);
    assert_eq!(progress(&[d(0, 0, None, DownloadState::Queued)]), None, "no size, no fraction");
    let p = progress(&[d(1000, 250, Some(5), DownloadState::Downloading), d(3000, 3000, Some(9), DownloadState::Queued)])
        .unwrap();
    assert_eq!((p.fraction, p.size, p.size_left, p.time_left_ms, p.downloading), (0.1875, 4000, 3250, Some(9), true));
    let stuck = progress(&[d(100, 500, None, DownloadState::Paused)]).unwrap();
    assert_eq!((stuck.fraction, stuck.downloading), (0.0, false), "a size_left past the size is clamped");
}

#[test]
fn a_queue_time_left_reads_days_hours_minutes_and_seconds() {
    assert_eq!(time_left_of("00:00:05"), Some(5_000));
    assert_eq!(time_left_of("01:02:03.5"), Some(3_723_500));
    assert_eq!(time_left_of("2.00:00:00"), Some(172_800_000));
    for bad in ["", "12:00", "a:b:c", "1:2:3:4", "-1:00:00"] {
        assert_eq!(time_left_of(bad), None, "{bad}");
    }
}

#[test]
fn a_detail_reads_its_facts_its_downloads_and_its_requests() {
    let movie = r#"{"id":61,"title":"M","releaseDate":"2024-03-01","runtime":166,"voteAverage":8.2,
        "status":"Released","genres":[{"id":1,"name":"Science Fiction"},{"id":2,"name":"Adventure"}],
        "productionCompanies":[{"name":"Studio A"},{"name":"Studio B"}],
        "mediaInfo":{"status":3,"downloadStatus":[{"title":"M.2024","size":10,"sizeLeft":5,"status":"paused"}],
          "requests":[
            {"id":1,"status":3,"createdAt":"2026-01-01T00:00:00.000Z","requestedBy":{"displayName":"Sam"}},
            {"id":2,"status":2,"createdAt":"2026-02-01T00:00:00.000Z","requestedBy":{"displayName":"Alex"},
             "modifiedBy":{"displayName":"Owner"}}]}}"#;
    let (got, _) = run(vec![ok(movie)], |c, t| c.detail(&session(), MediaKind::Movie, 61, t));
    let detail = got.unwrap();
    assert_eq!(detail.facts, TitleFacts {
        rating: Some(8.2), runtime_min: Some(166), genres: vec!["Science Fiction".into(), "Adventure".into()],
        released: Some("2024-03-01".into()), studio: Some("Studio A".into()), tmdb_status: Some("Released".into()),
    });
    assert_eq!(detail.downloads.len(), 1);
    assert_eq!(detail.downloads[0].state, DownloadState::Paused);
    let who: Vec<_> = detail.requests.iter().map(|r| (r.id, r.requested_by.as_deref(), r.status)).collect();
    assert_eq!(who, [(2, Some("Alex"), RequestStatus::Approved), (1, Some("Sam"), RequestStatus::Declined)],
        "newest first, each with the title's own kind and id");
    assert!(detail.requests.iter().all(|r| r.tmdb == 61 && r.kind == MediaKind::Movie
        && r.availability == Availability::Processing && r.downloads.is_empty()));

    let show = r#"{"id":62,"name":"S","firstAirDate":"2020-05-05","episodeRunTime":[45,50],"voteAverage":0,
        "networks":[{"name":"Net"}],"seasons":[]}"#;
    let (got, _) = run(vec![ok(show)], |c, t| c.detail(&session(), MediaKind::Tv, 62, t));
    let facts = got.unwrap().facts;
    assert_eq!((facts.runtime_min, facts.studio.as_deref(), facts.released.as_deref(), facts.rating),
        (Some(45), Some("Net"), Some("2020-05-05"), None), "nobody voted: no rating");
}

#[test]
fn a_decision_posts_to_the_requests_own_route() {
    for (decision, verb) in [(Decision::Approve, "approve"), (Decision::Decline, "decline")] {
        let (got, seen) = run(vec![ok("{}")], |c, t| c.decide(&session(), 9, decision, t));
        assert_eq!(got, Ok(()));
        assert_eq!((seen[0].method, seen[0].url.clone(), seen[0].body.clone()),
            ("POST", format!("http://127.0.0.1:5055/api/v1/request/9/{verb}"), None));
    }
    let (got, _) = run(vec![status(409)], |c, t| c.decide(&session(), 9, Decision::Approve, t));
    assert_eq!(got, Err(SeerrError::AlreadyRequested), "no longer pending");
    let (got, seen) = run(vec![], |c, t| c.decide(&session(), 0, Decision::Approve, t));
    assert_eq!((got, seen.len()), (Err(SeerrError::NotFound), 0));
}

#[test]
fn only_a_request_manager_decides_and_a_viewer_sees_everybodys_requests() {
    let user = |permissions| SeerrUser { id: 1, name: String::new(), permissions };
    assert!(!user(32).may_manage_requests() && !user(32).sees_all_requests(), "Request alone");
    assert!(!user(16384).may_manage_requests() && user(16384).sees_all_requests(), "View Requests");
    assert!(!user(8192).may_manage_requests() && !user(8192).sees_all_requests(), "Advanced Requests");
    for bits in [2, 16, 2 | 32] {
        assert!(user(bits).may_manage_requests() && user(bits).sees_all_requests(), "{bits}");
    }
}

#[test]
fn a_request_posts_the_kind_the_id_and_a_shows_seasons() {
    let (got, seen) = run(vec![Ok(SeerrResponse { status: 201, ..Default::default() })],
        |c, t| c.request(&session(), MediaKind::Movie, 61, &[], &RequestOptions::default(), t));
    assert_eq!(got, Ok(()));
    assert_eq!((seen[0].method, seen[0].url.as_str()), ("POST", "http://127.0.0.1:5055/api/v1/request"));
    assert_eq!(body_json(&seen[0]), json!({ "mediaType": "movie", "mediaId": 61 }));
    let (_, seen) = run(vec![Ok(SeerrResponse { status: 201, ..Default::default() })],
        |c, t| c.request(&session(), MediaKind::Tv, 62, &[2, 3], &RequestOptions::default(), t));
    assert_eq!(body_json(&seen[0]), json!({ "mediaType": "tv", "mediaId": 62, "seasons": [2, 3] }));
    let (got, seen) = run(vec![], |c, t| c.request(&session(), MediaKind::Tv, 62, &[], &RequestOptions::default(), t));
    assert!(got.is_err() && seen.is_empty(), "a show request with no season never goes out");
}

/// A request names only the choices it was given: each one left out is the server's to make.
#[test]
fn a_request_names_the_server_profile_and_folder_it_was_given_and_nothing_else() {
    let options = RequestOptions { server: Some(1), profile: Some(7), folder: Some("/media/tv".into()) };
    let (_, seen) = run(vec![Ok(SeerrResponse { status: 201, ..Default::default() })],
        |c, t| c.request(&session(), MediaKind::Tv, 62, &[1], &options, t));
    assert_eq!(body_json(&seen[0]), json!({ "mediaType": "tv", "mediaId": 62, "seasons": [1],
        "serverId": 1, "profileId": 7, "rootFolder": "/media/tv" }));
    let only_profile = RequestOptions { profile: Some(4), ..Default::default() };
    let (_, seen) = run(vec![Ok(SeerrResponse { status: 201, ..Default::default() })],
        |c, t| c.request(&session(), MediaKind::Movie, 61, &[], &only_profile, t));
    assert_eq!(body_json(&seen[0]), json!({ "mediaType": "movie", "mediaId": 61, "profileId": 4 }));
}

/// The servers a request can go to: Radarr's for a movie, Sonarr's for a show, with their defaults.
#[test]
fn the_servers_of_a_kind_are_read_with_their_defaults() {
    let body = r#"[{"id":0,"name":"s00000001","is4k":false,"isDefault":true,"activeDirectory":"/movies",
        "activeProfileId":4,"activeTags":[]},
        {"id":1,"name":"s00000002","is4k":true,"isDefault":true,"activeDirectory":"/movies4k","activeProfileId":5},
        {"name":"no id"}]"#;
    let (got, seen) = run(vec![ok(body)], |c, t| c.servers(&session(), MediaKind::Movie, t));
    assert_eq!(seen[0].url, "http://127.0.0.1:5055/api/v1/service/radarr");
    let servers = got.unwrap();
    assert_eq!(servers.len(), 2, "a row with no id is dropped");
    assert_eq!(servers[0], ArrServer { id: 0, name: "s00000001".into(), is4k: false, is_default: true,
        profile: Some(4), folder: Some("/movies".into()), anime_profile: None, anime_folder: None });
    assert!(servers[1].is4k);
    let (_, seen) = run(vec![ok("[]")], |c, t| c.servers(&session(), MediaKind::Tv, t));
    assert_eq!(seen[0].url, "http://127.0.0.1:5055/api/v1/service/sonarr");
    let (got, _) = run(vec![ok("{}")], |c, t| c.servers(&session(), MediaKind::Tv, t));
    assert_eq!(got, Err(SeerrError::Unreadable), "a list is an array");
}

/// One server's quality profiles and root folders, in the server's order.
#[test]
fn a_servers_profiles_and_folders_are_read() {
    let body = r#"{"server":{"id":2},"profiles":[{"id":1,"name":"s00000011"},{"id":6,"name":"s00000012"},{"id":9}],
        "rootFolders":[{"id":1,"path":"/tv","freeSpace":1099511627776,"totalSpace":2},{"id":2,"path":"/anime"},{"id":3}],
        "tags":[]}"#;
    let (got, seen) = run(vec![ok(body)], |c, t| c.server_options(&session(), MediaKind::Tv, 2, t));
    assert_eq!(seen[0].url, "http://127.0.0.1:5055/api/v1/service/sonarr/2");
    let options = got.unwrap();
    assert_eq!(options.profiles, [QualityProfile { id: 1, name: "s00000011".into() },
        QualityProfile { id: 6, name: "s00000012".into() }], "a profile with no name is dropped");
    assert_eq!(options.folders, [RootFolder { path: "/tv".into(), free: Some(1 << 40) },
        RootFolder { path: "/anime".into(), free: None }]);
}

/// The default server is the non-4K one marked default, else the first non-4K one; an anime show
/// takes a Sonarr server's anime profile and folder when it has them.
#[test]
fn the_default_server_and_its_defaults_follow_seerrs_rules() {
    let server = |id, is4k, is_default| ArrServer { id, is4k, is_default, ..Default::default() };
    assert_eq!(default_server(&[server(0, true, true), server(1, false, false), server(2, false, true)]).map(|s| s.id), Some(2));
    assert_eq!(default_server(&[server(0, true, true), server(1, false, false)]).map(|s| s.id), Some(1));
    assert_eq!(default_server(&[server(0, true, true)]), None, "a 4K server is never this client's");
    let sonarr = ArrServer { profile: Some(1), folder: Some("/tv".into()), anime_profile: Some(3),
        anime_folder: Some("/anime".into()), ..Default::default() };
    assert_eq!(sonarr.defaults(false), (Some(1), Some("/tv")));
    assert_eq!(sonarr.defaults(true), (Some(3), Some("/anime")));
    let plain = ArrServer { profile: Some(1), folder: Some("/tv".into()), ..Default::default() };
    assert_eq!(plain.defaults(true), (Some(1), Some("/tv")), "no anime pair: the ordinary one");
}

/// A show under TMDB's anime keyword reads as anime; a movie never does.
#[test]
fn a_shows_anime_keyword_is_read() {
    let body = r#"{"id":71,"name":"s00000071","keywords":[{"id":1,"name":"x"},{"id":210024,"name":"anime"}]}"#;
    let (got, _) = run(vec![ok(body)], |c, t| c.detail(&session(), MediaKind::Tv, 71, t));
    assert!(got.unwrap().anime);
    let (got, _) = run(vec![ok(r#"{"id":72,"name":"s00000072","keywords":[{"id":5}]}"#)],
        |c, t| c.detail(&session(), MediaKind::Tv, 72, t));
    assert!(!got.unwrap().anime);
    let (got, _) = run(vec![ok(r#"{"id":73,"title":"s00000073","keywords":[{"id":210024}]}"#)],
        |c, t| c.detail(&session(), MediaKind::Movie, 73, t));
    assert!(!got.unwrap().anime);
}

/// The permission bits say whether this user may choose a request's server, profile and folder.
#[test]
fn only_an_admin_or_an_advanced_requester_may_choose_options() {
    let (got, _) = run(vec![Ok(SeerrResponse { status: 200, body: br#"{"id":7,"permissions":32}"#.to_vec(),
        cookies: vec!["connect.sid=x".into()] })], |c, t| c.sign_in("t", t));
    let (_, user) = got.unwrap();
    assert_eq!(user.permissions, 32);
    assert!(!user.may_choose_options(), "Request alone");
    for bits in [PERMISSION_ADMIN, PERMISSION_MANAGE_REQUESTS, PERMISSION_REQUEST_ADVANCED | 32] {
        assert!(SeerrUser { id: 1, name: String::new(), permissions: bits }.may_choose_options(), "{bits}");
    }
}

#[test]
fn statuses_map_to_their_errors() {
    for (code, want) in [(401, SeerrError::Unauthorized), (403, SeerrError::Forbidden), (404, SeerrError::NotFound),
        (409, SeerrError::AlreadyRequested), (500, SeerrError::Other(500))] {
        let (got, _) = run(vec![status(code)], |c, t| c.me(&session(), t));
        assert_eq!(got.unwrap_err(), want, "HTTP {code}");
    }
    let (got, _) = run(vec![ok("not json")], |c, t| c.me(&session(), t));
    assert_eq!(got.unwrap_err(), SeerrError::Unreadable);
    let (got, _) = run(vec![Err(SeerrError::Transport)], |c, t| c.me(&session(), t));
    assert_eq!(got.unwrap_err(), SeerrError::Transport);
}

#[test]
fn an_address_is_normalized_or_refused() {
    for (typed, want) in [
        ("192.168.1.10:5055", "http://192.168.1.10:5055"),
        ("  http://seerr.lan:5055/  ", "http://seerr.lan:5055"),
        ("HTTPS://requests.example.com", "https://requests.example.com"),
        ("https://example.com/seerr/", "https://example.com/seerr"),
        ("https://example.com/seerr/api/v1/", "https://example.com/seerr"),
        ("http://[fd00::5]:5055", "http://[fd00::5]:5055"),
        ("http://[fd00::5]", "http://[fd00::5]"),
    ] {
        assert_eq!(normalize_address(typed).as_deref(), Some(want), "{typed:?}");
    }
    for typed in ["", "   ", "ftp://host", "http://", "http://host:0", "http://host:70000", "http://host:", "http://host:5o",
        "http://user:pw@host", "http://host/?q=1", "http://host/#x", "http://ho st", "http://[fd00::5", "http://h_st",
        "http://[]:1", "http://[fd00::5]x"] {
        assert_eq!(normalize_address(typed), None, "{typed:?}");
    }
}

#[test]
fn only_a_loopback_trigger_replaces_the_configured_address() {
    assert_eq!(resolve_base(Some("http://127.0.0.1:32499/"), "https://seerr.example.com").as_deref(),
        Some("http://127.0.0.1:32499"));
    assert_eq!(resolve_base(Some("http://localhost:1"), "").as_deref(), Some("http://localhost:1"));
    assert_eq!(resolve_base(Some("https://evil.example.com"), "seerr.lan:5055").as_deref(),
        Some("http://seerr.lan:5055"), "a non-loopback trigger is never followed");
    assert_eq!(resolve_base(None, "not a url"), None);
}

#[test]
fn a_dev_build_refuses_a_request_write_unless_the_stand_in_or_the_live_switch_is_armed() {
    let url = "http://127.0.0.1:5055/api/v1/request";
    assert_eq!(permit_write("POST", url, true, false, false), Err(SeerrError::Refused));
    assert_eq!(permit_write("POST", url, true, true, false), Ok(()));
    assert_eq!(permit_write("POST", url, true, false, true), Ok(()));
    assert_eq!(permit_write("POST", url, false, false, false), Ok(()), "a release build always sends");
    assert_eq!(permit_write("POST", "http://h/api/v1/auth/plex", true, false, false), Ok(()), "signing in is no write");
    assert_eq!(permit_write("GET", "http://h/api/v1/request?take=20", true, false, false), Ok(()), "reading the list is not");
    for write in ["http://h/api/v1/request/9/approve", "http://h/seerr/api/v1/request/12/decline",
        "http://h/api/v1/request/9/retry"]
    {
        assert_eq!(permit_write("POST", write, true, false, false), Err(SeerrError::Refused), "{write}");
        assert_eq!(permit_write("POST", write, true, true, false), Ok(()), "{write}");
    }
    assert_eq!(permit_write("GET", "http://h/api/v1/request/9", true, false, false), Ok(()), "reading one is not");
    assert_eq!(permit_write("POST", "http://h/api/v1/requests", true, false, false), Ok(()), "another route");
}

#[test]
fn a_log_line_names_the_path_shape_and_never_the_host_or_the_query() {
    let got = line("GET", "https://seerr.household.example:5055/api/v1/search?query=s0000abcd&page=1", Ok(500), false);
    assert_eq!(got, "seerr: GET /api/v1/search -> HTTP 500");
    let got = line("POST", "http://10.0.0.9:5055/api/v1/request", Err(()), true);
    assert_eq!(got, "seerr: LIVE POST /api/v1/request -> no answer");
    assert!(!line("GET", "http://10.0.0.9:5055/api/v1/tv/12345678", Ok(404), false).contains("12345678"),
        "a long id folds");
}

#[test]
fn a_tmdb_image_url_is_built_only_from_a_plain_path() {
    assert_eq!(tmdb_image("/abc_D-1.jpg", "w342").as_deref(), Some("https://image.tmdb.org/t/p/w342/abc_D-1.jpg"));
    for bad in ["", "/", "abc.jpg", "/a?b=1", "/a b.jpg", "/../x%2e"] {
        assert_eq!(tmdb_image(bad, "w342"), None, "{bad:?}");
    }
}

#[test]
fn a_session_and_a_user_never_print_their_secrets() {
    assert_eq!(format!("{:?}", session()), "SeerrSession(<cookie>)");
    let user = SeerrUser { id: 3, name: "s00000003".into(), permissions: 2 };
    assert!(!format!("{user:?}").contains("s00000003"));
}
