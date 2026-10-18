use super::*;

const ALL: [TileState; 6] = [
    TileState::Available,
    TileState::ComingSoon,
    TileState::ComingSoonMatched,
    TileState::NotOnAnyServer,
    TileState::ServerOffline,
    TileState::Unknown,
];

/// Plan §1.9: a matched, reachable copy opens Detail; every other state has only the catalog page.
#[test]
fn the_go_row_follows_the_state() {
    for state in ALL {
        let rows = rows_for(state, Kind::Movie);
        let want = match state {
            TileState::Available | TileState::ComingSoonMatched => HoldRowId::GoToDetail,
            _ => HoldRowId::GoToCatalog,
        };
        assert_eq!(rows[0].id, want, "{state:?}");
        assert!(!rows[0].destructive, "{state:?}: the opening row is never destructive");
    }
}

#[test]
fn every_state_offers_remove_last_and_destructive() {
    for state in ALL {
        let rows = rows_for(state, Kind::Movie);
        assert_eq!(rows.len(), 2, "{state:?}");
        let last = rows.last().unwrap();
        assert_eq!(last.id, HoldRowId::Remove, "{state:?}");
        assert!(last.destructive, "{state:?}");
        assert_eq!(rows.iter().filter(|r| r.destructive).count(), 1, "{state:?}");
        assert!(rows.iter().all(|r| !r.label.is_empty()), "{state:?}");
    }
}

#[test]
fn rows_map_to_results() {
    assert_eq!(HoldRowId::Remove.result(), HoldResult::Remove);
    assert_eq!(HoldRowId::GoToDetail.result(), HoldResult::OpenDetail);
    assert_eq!(HoldRowId::GoToCatalog.result(), HoldResult::OpenCatalog);
}

/// The "go" row names the tile's kind: a show reads "Go to Show", never "Go to Movie".
#[test]
fn the_go_row_names_the_kind() {
    use plx_platform::i18n::{language_on_this_thread_for_test, Preference};
    let _guard = language_on_this_thread_for_test(Preference::En);
    for state in ALL {
        assert_eq!(rows_for(state, Kind::Movie)[0].label, "Go to Movie", "{state:?}");
        assert_eq!(rows_for(state, Kind::Show)[0].label, "Go to Show", "{state:?}");
    }
}
