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
fn my_requests_pages_by_skip_and_reads_kind_status_and_seasons() {
    let body = r#"{"pageInfo":{"pages":4,"page":2},"results":[
        {"id":5,"status":1,"type":"tv","media":{"tmdbId":51,"mediaType":"tv","status":2},
         "seasons":[{"seasonNumber":2},{"seasonNumber":3}]},
        {"id":6,"status":2,"media":{"tmdbId":52,"mediaType":"movie","status":3}},
        {"id":7,"status":1,"media":{"mediaType":"movie"}}]}"#;
    let (got, seen) = run(vec![ok(body)], |c, t| c.my_requests(&session(), 7, 2, t));
    assert_eq!(seen[0].url,
        "http://127.0.0.1:5055/api/v1/request?take=20&skip=20&filter=all&sort=added&requestedBy=7");
    let page = got.unwrap();
    assert_eq!((page.page, page.pages), (2, 4));
    assert_eq!(page.items, [
        MyRequest { id: 5, kind: MediaKind::Tv, tmdb: 51, status: RequestStatus::PendingApproval,
            availability: Availability::Pending, seasons: vec![2, 3] },
        MyRequest { id: 6, kind: MediaKind::Movie, tmdb: 52, status: RequestStatus::Approved,
            availability: Availability::Processing, seasons: vec![] },
    ], "a row with no tmdb id is dropped");
}

#[test]
fn a_request_posts_the_kind_the_id_and_a_shows_seasons() {
    let (got, seen) = run(vec![Ok(SeerrResponse { status: 201, ..Default::default() })],
        |c, t| c.request(&session(), MediaKind::Movie, 61, &[], t));
    assert_eq!(got, Ok(()));
    assert_eq!((seen[0].method, seen[0].url.as_str()), ("POST", "http://127.0.0.1:5055/api/v1/request"));
    assert_eq!(body_json(&seen[0]), json!({ "mediaType": "movie", "mediaId": 61 }));
    let (_, seen) = run(vec![Ok(SeerrResponse { status: 201, ..Default::default() })],
        |c, t| c.request(&session(), MediaKind::Tv, 62, &[2, 3], t));
    assert_eq!(body_json(&seen[0]), json!({ "mediaType": "tv", "mediaId": 62, "seasons": [2, 3] }));
    let (got, seen) = run(vec![], |c, t| c.request(&session(), MediaKind::Tv, 62, &[], t));
    assert!(got.is_err() && seen.is_empty(), "a show request with no season never goes out");
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
    let user = SeerrUser { id: 3, name: "s00000003".into() };
    assert!(!format!("{user:?}").contains("s00000003"));
}
