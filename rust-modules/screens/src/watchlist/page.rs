//! The page's state and its input/output alphabets (plan §2.4 W45).
//!
//! CONTRACT FREEZE: [`WatchlistScreen`] is constructed from a snapshot of `data::watchlist` rows
//! plus the options value; it is not yet a `Screen` impl (W45P mounts it). The app feeds it
//! [`WatchlistIn`] and routes the [`WatchlistOut`] it returns.
use super::options::WatchlistOptionsState;
use plx_data::watchlist::model::{ServerMatch, TileState, WatchRow};

/// One row of the snapshot the page draws: the stored row, its committed tile state and the
/// matched copy a press opens (if any). Built by the app from `WatchlistView`.
#[derive(Clone, Debug, PartialEq)]
pub struct WatchTile {
    pub row: WatchRow,
    pub state: TileState,
    pub copy: Option<ServerMatch>,
}

/// What the page asks the app to do. The app owns routing (a screen names no sibling).
#[derive(Clone, Debug, PartialEq)]
pub enum WatchlistOut {
    /// OK on a tile with a reachable matched copy: the Detail page of that (server, ratingKey).
    OpenDetail(ServerMatch),
    /// OK on a tile with no playable copy: the catalog page of this guid.
    /// TODO(W6): the app has no `ContentArg::Catalog` yet; until W6 lands the app drops this.
    OpenCatalog(String),
    /// Hold on the tile at `index` (in the shown order).
    Hold { index: usize, guid: String, state: TileState, copy: Option<ServerMatch> },
    /// OK on the Sort & Filter pill.
    OpenOptions,
    /// BACK.
    Back,
}

/// What the app tells the page.
#[derive(Clone, Debug, PartialEq)]
pub enum WatchlistIn {
    /// A new snapshot (already ordered and filtered by the options).
    Rows(Vec<WatchTile>),
    /// This guid left the list: the DS4 landing runs and focus moves to the next title.
    Removed(String),
    /// The Sort & Filter value changed.
    Options(WatchlistOptionsState),
}

/// The page's state. Fields are private; W45P grows them (focus, scroll, landing).
pub struct WatchlistScreen {
    tiles: Vec<WatchTile>,
    options: WatchlistOptionsState,
    focus: usize,
}

impl WatchlistScreen {
    pub fn new(rows: Vec<WatchTile>, options: WatchlistOptionsState) -> Self {
        Self { tiles: rows, options, focus: 0 }
    }

    pub fn tiles(&self) -> &[WatchTile] {
        &self.tiles
    }

    pub fn options(&self) -> &WatchlistOptionsState {
        &self.options
    }

    /// The focused tile's index in the shown order, if there is one.
    pub fn focused(&self) -> Option<usize> {
        (self.focus < self.tiles.len()).then_some(self.focus)
    }

    /// Take an input. STUB (W45P): `Rows` and `Options` replace the state; `Removed` drops the
    /// tile and leaves focus on the next title (clamped). The DS4 landing animation is W45P's.
    pub fn input(&mut self, ev: WatchlistIn) {
        match ev {
            WatchlistIn::Rows(rows) => self.tiles = rows,
            WatchlistIn::Options(options) => self.options = options,
            WatchlistIn::Removed(guid) => self.tiles.retain(|t| t.row.guid != guid),
        }
        self.focus = self.focus.min(self.tiles.len().saturating_sub(1));
    }

    /// OK on the focused tile, per the plan §1.9 table.
    pub fn press(&self) -> Option<WatchlistOut> {
        let t = self.tiles.get(self.focused()?)?;
        Some(match (t.state, t.copy) {
            (TileState::Available | TileState::ComingSoonMatched, Some(copy)) => WatchlistOut::OpenDetail(copy),
            _ => WatchlistOut::OpenCatalog(t.row.guid.clone()),
        })
    }

    /// Hold on the focused tile.
    pub fn hold(&self) -> Option<WatchlistOut> {
        let index = self.focused()?;
        let t = &self.tiles[index];
        Some(WatchlistOut::Hold { index, guid: t.row.guid.clone(), state: t.state, copy: t.copy })
    }
}
