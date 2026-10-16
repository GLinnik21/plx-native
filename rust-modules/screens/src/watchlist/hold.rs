//! The grid's hold menu (plan §1.9): which rows a tile in each state offers, and what a press on
//! one asks of the app. The rows are the Watchlist's own; `item_menu` presents them for
//! `ItemMenuKind::Catalog` and the app maps the results onto its actions.
use plx_data::watchlist::model::TileState;
use plx_ui::icons::Icon;

/// A row's identity (hand-assigned, never a position, like `item_menu::ItemRow`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HoldRowId {
    /// Go to the matched copy's Detail page.
    GoToDetail,
    /// Go to the catalog page.
    GoToCatalog,
    /// Remove from Watchlist.
    Remove,
}

/// One hold-menu row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemMenuRow {
    pub id: HoldRowId,
    pub label: &'static str,
    pub icon: Icon,
    /// Remove ends a membership: `Row::destructive`, never the opening row.
    pub destructive: bool,
}

/// What a press on a row asks of the app.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HoldResult {
    Remove,
    OpenDetail,
    OpenCatalog,
    Dismiss,
}

impl HoldRowId {
    pub fn result(self) -> HoldResult {
        match self {
            HoldRowId::GoToDetail => HoldResult::OpenDetail,
            HoldRowId::GoToCatalog => HoldResult::OpenCatalog,
            HoldRowId::Remove => HoldResult::Remove,
        }
    }
}

/// The rows for a tile in `state`: a "go" row (Detail when a reachable copy is matched, else the
/// catalog page), then the destructive Remove row. Per the §1.9 table every state offers Remove.
/// TODO(W45H): the catalog "go" label needs its own key (the integrator adds it to the tables);
/// until then it reads the movie label.
pub fn rows_for(state: TileState) -> Vec<ItemMenuRow> {
    let go = match state {
        TileState::Available | TileState::ComingSoonMatched => {
            (HoldRowId::GoToDetail, plx_platform::i18n::msg::browse_menu_go_movie())
        }
        TileState::ComingSoon | TileState::NotOnAnyServer | TileState::ServerOffline | TileState::Unknown => {
            (HoldRowId::GoToCatalog, plx_platform::i18n::msg::browse_menu_go_movie())
        }
    };
    vec![
        ItemMenuRow { id: go.0, label: go.1, icon: Icon::Episode, destructive: false },
        ItemMenuRow {
            id: HoldRowId::Remove,
            label: plx_platform::i18n::msg::watchlist_action_remove(),
            icon: Icon::Close,
            destructive: true,
        },
    ]
}
