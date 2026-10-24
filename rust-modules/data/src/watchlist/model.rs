//! Watchlist rows, the guid-to-server match table and the six tile states (plan §1.5, §1.9).
use plx_plex::plex::{ListingFacts, ServerId};
use std::collections::HashMap;

/// A watchlist title's kind. Collections and other kinds never become rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Movie,
    Show,
}

/// One stored row. `hex` is the 24-hex discover id decoded once; `guid` is `plex://movie|show/<hex>`.
#[derive(Clone, Debug, PartialEq)]
pub struct WatchRow {
    pub guid: String,
    pub hex: [u8; 12],
    pub kind: Kind,
    pub title: String,
    /// `titleSort` when the service sent one, else empty (the comparator falls back to `title`).
    pub title_sort: String,
    pub year: Option<i32>,
    /// `watchlistedAt`, unix seconds.
    pub added_at: Option<i64>,
    /// `originallyAvailableAt` as `YYYY-MM-DD`; `None` sorts last in both directions.
    pub release: Option<String>,
    pub rating: Option<f32>,
    /// In the `released` set (false = coming soon).
    pub released: bool,
    /// The row's server-side position in the default order, so Date Added ascending is the exact
    /// reverse of what the server sent.
    pub order: u32,
    /// The Discover item's poster (`thumb`): an absolute URL the poster store hands to a reachable
    /// server's `/photo/:/transcode?url=` proxy (plan §0, §3), or empty when the service sent none.
    pub thumb: String,
}

/// A matched item's `UltraBlurColors` corners in `Painter::ambient`'s order, one byte per channel
/// (the wire's hex colours are bytes, so nothing is lost and the match stays `Eq`).
pub type Blur = [[u8; 3]; 4];

/// One server's copy of a guid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServerMatch {
    pub server_id: ServerId,
    /// This server's ratingKey for the title, parsed once.
    pub rating_key: i64,
    /// The copy's UltraBlur colours when the guid lookup's row carried them (plan §1.6): the
    /// ground the page keys while this title is focused.
    pub blur: Option<Blur>,
}

impl ServerMatch {
    /// A copy with no colours.
    pub fn new(server_id: ServerId, rating_key: i64) -> Self {
        Self { server_id, rating_key, blur: None }
    }
}

/// What one server answered for one guid, so "absent" (answered, not held) stays apart from
/// "silent" (no answer yet).
#[derive(Clone, Debug, PartialEq)]
pub enum Asked {
    /// The server holds the title: the copy a press opens, and what the lookup's row says of it for
    /// a caption (genre, video, ratings, length, counts).
    Held(ServerMatch, ListingFacts),
    Absent,
}

/// Per profile, in memory: guid to what each asked server said.
#[derive(Clone, Debug, Default)]
pub struct MatchTable {
    pub by_guid: HashMap<String, Vec<(ServerId, Asked)>>,
    /// `plex::servers::roster_gen()` the table was built against; a move forces a full re-match.
    pub roster_gen: u32,
}

/// The reachability facts `tile_state` reads (`plex::servers::probe_result` behind the
/// production impl; a stub in tests).
pub trait Reach {
    fn reachable(&self, server: ServerId) -> bool;
    /// Every server the match table was asked, in roster order.
    fn roster(&self) -> Vec<ServerId>;
    /// How early a held copy on this server is preferred (plan §1.5): `0` the current server,
    /// `1` a server this account owns, `2` a share. Ties keep roster order; the default ranks every
    /// server alike.
    fn copy_rank(&self, _server: ServerId) -> u8 {
        2
    }
}

/// The six drawn states (plan §1.9 table). OK and hold read the committed `row.state`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TileState {
    /// Released and matched on at least one Reachable server: PMS tick/progress, opens Detail.
    Available,
    /// Not released and not matched: clock mark, opens the catalog page.
    ComingSoon,
    /// Not released but matched on a Reachable server (Q6): clock kept, opens Detail.
    ComingSoonMatched,
    /// Released and every server answered "absent": circle-slash mark, washed, opens the catalog page.
    NotOnAnyServer,
    /// Matched earlier this session, every matching server now Unreachable: server-slash mark.
    ServerOffline,
    /// No match and some server silent: no mark, no wash, excluded from "On your servers".
    Unknown,
}

/// Derive a row's state. Pure; run at commit and when `facts_gen`/reachability moves.
/// Membership in flight (the intent ledger) is NOT a tile state: the ledger flips the row set.
pub fn tile_state(row: &WatchRow, table: &MatchTable, reach: &dyn Reach) -> TileState {
    let answers: &[(ServerId, Asked)] = table.by_guid.get(&row.guid).map_or(&[], |v| v.as_slice());
    let held = || answers.iter().filter(|(_, a)| matches!(a, Asked::Held(..)));
    if held().any(|(sid, _)| reach.reachable(*sid)) {
        return if row.released { TileState::Available } else { TileState::ComingSoonMatched };
    }
    if held().next().is_some() {
        // Matched earlier, every matching server is now Unreachable.
        return TileState::ServerOffline;
    }
    if !row.released {
        return TileState::ComingSoon;
    }
    // Released, no copy: "absent" only once every server on the roster has answered.
    let roster = reach.roster();
    let all_absent = !roster.is_empty()
        && roster.iter().all(|sid| answers.iter().any(|(s, a)| s == sid && matches!(a, Asked::Absent)));
    if all_absent {
        TileState::NotOnAnyServer
    } else {
        TileState::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    struct Stub {
        up: RefCell<Vec<ServerId>>,
        roster: Vec<ServerId>,
    }
    impl Reach for Stub {
        fn reachable(&self, s: ServerId) -> bool {
            self.up.borrow().contains(&s)
        }
        fn roster(&self) -> Vec<ServerId> {
            self.roster.clone()
        }
    }

    fn sid(n: usize) -> ServerId {
        ServerId::from_raw(n as u16)
    }

    fn row(released: bool) -> WatchRow {
        WatchRow {
            guid: "plex://movie/aaaaaaaaaaaaaaaaaaaaaaaa".into(),
            hex: [0xaa; 12],
            kind: Kind::Movie,
            title: "T".into(),
            title_sort: String::new(),
            year: None,
            added_at: None,
            release: None,
            rating: None,
            released,
            order: 0,
            thumb: String::new(),
        }
    }

    fn held(s: ServerId) -> (ServerId, Asked) {
        (s, Asked::Held(ServerMatch::new(s, 7), ListingFacts::default()))
    }

    fn table(r: &WatchRow, v: Vec<(ServerId, Asked)>) -> MatchTable {
        let mut t = MatchTable::default();
        t.by_guid.insert(r.guid.clone(), v);
        t
    }

    fn stub(up: &[ServerId], roster: &[ServerId]) -> Stub {
        Stub { up: RefCell::new(up.to_vec()), roster: roster.to_vec() }
    }

    #[test]
    fn available_when_released_and_held_on_a_reachable_server() {
        let s = sid(0);
        let r = row(true);
        assert_eq!(tile_state(&r, &table(&r, vec![held(s)]), &stub(&[s], &[s])), TileState::Available);
    }

    #[test]
    fn coming_soon_unmatched_and_matched() {
        let s = sid(0);
        let r = row(false);
        assert_eq!(tile_state(&r, &MatchTable::default(), &stub(&[s], &[s])), TileState::ComingSoon);
        assert_eq!(
            tile_state(&r, &table(&r, vec![(s, Asked::Absent)]), &stub(&[s], &[s])),
            TileState::ComingSoon
        );
        assert_eq!(
            tile_state(&r, &table(&r, vec![held(s)]), &stub(&[s], &[s])),
            TileState::ComingSoonMatched
        );
    }

    #[test]
    fn not_on_any_server_needs_every_roster_server_to_answer_absent() {
        let s = sid(0);
        let r = row(true);
        let t = table(&r, vec![(s, Asked::Absent)]);
        assert_eq!(tile_state(&r, &t, &stub(&[s], &[s])), TileState::NotOnAnyServer);
        // An empty roster has asked nobody: unknown, not absent.
        assert_eq!(tile_state(&r, &t, &stub(&[], &[])), TileState::Unknown);
    }

    #[test]
    fn unknown_when_no_match_and_a_server_is_silent() {
        let s = sid(0);
        let r = row(true);
        assert_eq!(tile_state(&r, &MatchTable::default(), &stub(&[s], &[s])), TileState::Unknown);
    }

    #[test]
    fn server_offline_when_every_matching_server_is_unreachable() {
        let s = sid(0);
        let r = row(true);
        assert_eq!(tile_state(&r, &table(&r, vec![held(s)]), &stub(&[], &[s])), TileState::ServerOffline);
    }

    #[test]
    fn one_reachable_copy_among_several_is_enough() {
        let (a, b) = (sid(0), sid(1));
        let r = row(true);
        let t = table(&r, vec![held(a), (b, Asked::Absent)]);
        assert_eq!(tile_state(&r, &t, &stub(&[a], &[a, b])), TileState::Available);
        assert_eq!(tile_state(&r, &t, &stub(&[b], &[a, b])), TileState::ServerOffline);
        // One server absent, the other silent: not yet "on no server".
        let t = table(&r, vec![(a, Asked::Absent)]);
        assert_eq!(tile_state(&r, &t, &stub(&[a, b], &[a, b])), TileState::Unknown);
    }

    #[test]
    fn a_reachability_flip_rederives_without_touching_the_table() {
        let s = sid(0);
        let r = row(true);
        let t = table(&r, vec![held(s)]);
        let reach = stub(&[s], &[s]);
        assert_eq!(tile_state(&r, &t, &reach), TileState::Available);
        reach.up.borrow_mut().clear();
        assert_eq!(tile_state(&r, &t, &reach), TileState::ServerOffline);
        reach.up.borrow_mut().push(s);
        assert_eq!(tile_state(&r, &t, &reach), TileState::Available);
    }
}
