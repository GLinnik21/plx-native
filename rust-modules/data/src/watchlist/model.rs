//! Watchlist rows, the guid-to-server match table and the six tile states (plan §1.5, §1.9).
use plx_plex::plex::ServerId;
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
}

/// One server's copy of a guid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServerMatch {
    pub server_id: ServerId,
    /// This server's ratingKey for the title, parsed once.
    pub rating_key: i64,
}

/// What one server answered for one guid, so "absent" (answered, not held) stays apart from
/// "silent" (no answer yet).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Asked {
    Held(ServerMatch),
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
    /// Released and every server answered "absent": no mark, washed, opens the catalog page.
    NotOnAnyServer,
    /// Matched earlier this session, every matching server now Unreachable: server-slash mark.
    ServerOffline,
    /// No match and some server silent: no mark, no wash, excluded from "On your servers".
    Unknown,
}

/// Derive a row's state. Pure; run at commit and when `facts_gen`/reachability moves.
/// Membership in flight (the intent ledger) is NOT a tile state: the ledger flips the row set.
pub fn tile_state(row: &WatchRow, table: &MatchTable, reach: &dyn Reach) -> TileState {
    let _ = (row, table, reach);
    unimplemented!("W2a")
}
