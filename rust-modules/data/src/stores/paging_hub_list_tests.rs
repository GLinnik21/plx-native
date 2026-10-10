//! The hub list read in windows of hubs: the server is a closure that honours the paging headers
//! (or does not), counts its requests and can change the list under the reader.

use super::*;

type Container = plx_plex::plex::MediaContainer;

/// A hub of twelve cards: row `r` card `i` is keyed `r * 1000 + i`.
pub(crate) fn hub_json(r: usize) -> serde_json::Value {
    let cards: Vec<_> = (0..12).map(|i| serde_json::json!({
        "ratingKey": (r * 1000 + i).to_string(), "type": "movie", "title": "Movie", "thumb": "/poster"})).collect();
    serde_json::json!({"hubIdentifier": format!("movie.row{r}"), "title": format!("Row {r}"), "type": "movie",
        "key": format!("/hubs/sections/1/row{r}"), "more": true, "size": 12, "totalSize": 500, "Metadata": cards})
}

/// What the server does with the hub list it is asked for.
pub(crate) struct HubServer {
    /// The hubs, by row number; changed under a reader to move the list.
    pub rows: Vec<usize>,
    /// Whether the paging headers are honoured; a server that ignores them answers the whole list.
    pub honours: bool,
    /// Whether the answer carries `totalSize` and `offset`.
    pub reports: bool,
    pub requests: usize,
    /// The most hubs any one response carried.
    pub widest: usize,
    /// The last request was the unpaged one.
    pub last_whole: bool,
}

impl HubServer {
    pub(crate) fn new(rows: usize) -> Self {
        Self { rows: (0..rows).collect(), honours: true, reports: true, requests: 0, widest: 0, last_whole: false }
    }

    pub(crate) fn answer(&mut self, req: Option<PageReq>) -> Option<Container> {
        self.requests += 1;
        self.last_whole = req.is_none();
        let (start, rows) = match req {
            Some(req) if self.honours => {
                let start = req.start.min(self.rows.len());
                (start, &self.rows[start..(start + req.size).min(self.rows.len())])
            }
            _ => (0, &self.rows[..]),
        };
        self.widest = self.widest.max(rows.len());
        let mut container = serde_json::json!({"Hub": rows.iter().map(|&r| hub_json(r)).collect::<Vec<_>>()});
        if self.reports {
            container["totalSize"] = self.rows.len().into();
            container["offset"] = start.into();
        }
        Some(serde_json::from_value(container).unwrap())
    }
}

fn collect(ids: &mut Vec<String>, hubs: &[plx_plex::plex::Hub]) {
    ids.extend(hubs.iter().map(|hub| hub.hub_identifier.clone()));
}

fn read(server: &mut HubServer) -> Option<Vec<String>> {
    read_hub_list(|req| server.answer(req), Vec::new, collect)
}

fn ids(rows: impl Iterator<Item = usize>) -> Vec<String> { rows.map(|r| format!("movie.row{r}")).collect() }

#[test]
fn a_400_hub_list_is_read_in_windows_and_arrives_whole() {
    let mut server = HubServer::new(400);
    assert_eq!(read(&mut server).unwrap(), ids(0..400));
    assert!(server.widest <= HUB_WINDOW, "no response carries more than one window of hubs");
    assert_eq!(server.requests, 400usize.div_ceil(HUB_WINDOW));
}

#[test]
fn a_list_of_at_most_one_window_is_one_request() {
    for (rows, reports) in [(10, true), (10, false), (HUB_WINDOW, true), (0, true), (0, false)] {
        let mut server = HubServer::new(rows);
        server.reports = reports;
        assert_eq!(read(&mut server).unwrap(), ids(0..rows));
        assert_eq!(server.requests, 1, "{rows} hubs, totalSize reported: {reports}");
    }
}

#[test]
fn a_server_that_ignores_the_window_still_yields_the_whole_list() {
    for reports in [true, false] {
        let mut server = HubServer::new(100);
        server.honours = false;
        server.reports = reports;
        assert_eq!(read(&mut server).unwrap(), ids(0..100));
        assert_eq!(server.requests, 1, "the first answer is the list");
    }
    // exactly one window of hubs, ignored and unreported: the second window repeats it
    let mut server = HubServer::new(HUB_WINDOW);
    server.honours = false;
    server.reports = false;
    assert_eq!(read(&mut server).unwrap(), ids(0..HUB_WINDOW));
    assert!(server.last_whole, "a list that repeats itself is asked for whole");
}

#[test]
fn a_list_that_changes_between_windows_restarts_and_ends_complete() {
    // a hub appears at the front after the first window was read
    let mut server = HubServer::new(60);
    let mut calls = 0;
    let got = read_hub_list(|req| {
        calls += 1;
        if calls == 2 { server.rows.insert(0, 900); }
        server.answer(req)
    }, Vec::new, collect).unwrap();
    let mut want = ids(0..60);
    want.insert(0, "movie.row900".into());
    assert_eq!(got, want, "the restart read the list as it now is, once each, in order");
    assert!(!server.last_whole);

    // the same count in a new order: a hub is seen twice, which is also a restart
    let mut server = HubServer::new(60);
    let mut calls = 0;
    let got = read_hub_list(|req| {
        calls += 1;
        if calls == 2 { server.rows.rotate_left(1); }
        server.answer(req)
    }, Vec::new, collect).unwrap();
    let want: Vec<String> = ids(server.rows.iter().copied());
    assert_eq!(got, want, "no hub twice, none missing, in the order the list now holds");
}

#[test]
fn a_list_that_never_holds_still_is_asked_for_whole() {
    let mut server = HubServer::new(60);
    let mut grow = 1000;
    let got = read_hub_list(|req| {
        if req.is_some() { server.rows.push(grow); grow += 1; }
        server.answer(req)
    }, Vec::new, collect).unwrap();
    assert!(server.last_whole);
    assert_eq!(got.len(), server.rows.len());
}

#[test]
fn a_failed_window_fails_the_read() {
    let mut calls = 0;
    let mut server = HubServer::new(60);
    let got = read_hub_list(|req| { calls += 1; if calls == 2 { None } else { server.answer(req) } }, Vec::new, collect);
    assert!(got.is_none());
}
