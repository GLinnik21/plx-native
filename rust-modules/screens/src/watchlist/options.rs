//! The Sort & Filter value (plan §1.3, §1.10): what the pill summarises and the popover edits.
//! Filtering and ordering are client-side over the stored rows, so a change makes no request.
use super::page::WatchTile;
pub use plx_data::watchlist::order::SortKey;
use plx_data::watchlist::model::{Kind, TileState};
use plx_data::watchlist::order::{sorted_filtered, FilterSpec, Show, SortSpec};

/// The Type section: `Movies & Shows` / `Movies` / `TV Shows`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum TypeFilter {
    #[default]
    All,
    Movies,
    Shows,
}

/// The Show section: `All` / `Released` / `On your servers`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ShowFilter {
    #[default]
    All,
    Released,
    OnYourServers,
}

/// The whole Sort & Filter value. `Default` is the page's opening order: the service's own
/// (newest added first), every type, every row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WatchlistOptionsState {
    pub type_: TypeFilter,
    pub sort: SortKey,
    pub descending: bool,
    pub show: ShowFilter,
}

impl Default for WatchlistOptionsState {
    fn default() -> Self {
        Self { type_: TypeFilter::All, sort: SortKey::DateAdded, descending: true, show: ShowFilter::All }
    }
}

impl WatchlistOptionsState {
    /// The data layer's spelling of this value.
    pub fn specs(&self) -> (SortSpec, FilterSpec) {
        let kind = match self.type_ {
            TypeFilter::All => None,
            TypeFilter::Movies => Some(Kind::Movie),
            TypeFilter::Shows => Some(Kind::Show),
        };
        let show = match self.show {
            ShowFilter::All => Show::All,
            ShowFilter::Released => Show::Released,
            ShowFilter::OnYourServers => Show::OnYourServers,
        };
        (SortSpec { key: self.sort, desc: self.descending }, FilterSpec { kind, show })
    }

    /// The pill's `<summary>` half: "Default" when nothing is changed.
    /// STUB (W45O): returns the default word only; the non-default composition is W45O's.
    pub fn summary(&self) -> String {
        plx_platform::i18n::msg::watchlist_filter_sort_default().to_string()
    }

    /// `rows` filtered and ordered by this value, exactly as the data layer does it
    /// (`order::sorted_filtered`); "On your servers" reads each tile's committed state.
    pub fn apply(&self, rows: &[WatchTile]) -> Vec<WatchTile> {
        let (sort, filter) = self.specs();
        let data: Vec<_> = rows.iter().map(|t| t.row.clone()).collect();
        let playable = |i: usize| matches!(rows[i].state, TileState::Available | TileState::ComingSoonMatched);
        sorted_filtered(&data, &sort, &filter, &playable).into_iter().map(|i| rows[i].clone()).collect()
    }
}
