use super::*;
use crate::item_menu::{build_card, build_catalog, build_episode, build_season, ItemForm};
use plx_data::pms::PmsMovie;
use plx_data::watchlist::model::TileState;
use plx_media::route::DeckPress;
use plx_ui::form::{FormId, FormTable};
use plx_ui::widgets::PosterMark;

const HEX: [u8; 12] = [0xab; 12];

fn face(member: Membership, kind: Kind, from_episode: bool) -> Face {
    Face { hex: HEX, member, kind, from_episode }
}

struct Menu(FormTable<ItemRow, Action, Infallible>);
fn menu(form: ItemForm) -> Menu {
    let mut t = FormTable::new(crate::registry::BAND);
    t.set(form, None);
    Menu(t)
}
impl Menu {
    fn ids(&self) -> Vec<ItemRow> {
        (0..self.0.table.n_rows() as usize).filter_map(|i| self.0.id_at(i).copied()).collect()
    }
    fn action(&self, id: ItemRow) -> Action {
        let i = self.0.index_of(&id).unwrap_or_else(|| panic!("{id:?} not offered"));
        self.0.binding_at(i).expect("bound").action.clone()
    }
    fn offers(&self, id: ItemRow) -> bool {
        self.0.index_of(&id).is_some()
    }
    fn label(&self, id: ItemRow) -> String {
        let i = self.0.index_of(&id).expect("offered");
        self.0.table.sections[0].rows[i].label.clone()
    }
    fn destructive(&self, id: ItemRow) -> bool {
        let i = self.0.index_of(&id).expect("offered");
        self.0.table.sections[0].rows[i].destructive
    }
}

fn movie(kind: i32) -> PmsMovie {
    let mut m = PmsMovie::default();
    m.rk = "42".into();
    m.kind = kind;
    m.unwatched = true;
    m
}
fn card(kind: i32, f: Option<&Face>) -> Menu {
    menu(build_card(&movie(kind), false, DeckPress::Play, None, None, f))
}

#[test]
fn a_movie_card_offers_add_or_remove_by_membership() {
    let off = card(0, Some(&face(Membership::Off, Kind::Movie, false)));
    assert_eq!(off.label(ItemRow::WatchlistAdd), "Add to Watchlist");
    assert_eq!(off.action(ItemRow::WatchlistAdd), Action::WatchlistAdd { hex: HEX });
    assert!(!off.offers(ItemRow::WatchlistRemove));

    let on = card(0, Some(&face(Membership::On, Kind::Movie, false)));
    assert_eq!(on.label(ItemRow::WatchlistRemove), "Remove from Watchlist");
    assert_eq!(on.action(ItemRow::WatchlistRemove), Action::WatchlistRemove(guid_of(Kind::Movie, &HEX)));
    assert!(!on.offers(ItemRow::WatchlistAdd));
}

#[test]
fn unknown_membership_wears_the_add_face() {
    let m = card(0, Some(&face(Membership::Unknown, Kind::Movie, false)));
    assert!(m.offers(ItemRow::WatchlistAdd) && !m.offers(ItemRow::WatchlistRemove));
}

#[test]
fn an_unreachable_service_omits_the_row() {
    for kind in [0, 1, 3] {
        let m = card(kind, None);
        assert!(!m.offers(ItemRow::WatchlistAdd) && !m.offers(ItemRow::WatchlistRemove), "kind {kind}");
    }
    for m in [menu(build_episode("1", PosterMark::None, &[], None)), menu(build_season("1", PosterMark::None, None))] {
        assert!(!m.offers(ItemRow::WatchlistAdd) && !m.offers(ItemRow::WatchlistRemove));
    }
}

#[test]
fn an_episode_names_the_show() {
    let add = menu(build_episode("9", PosterMark::None, &[], Some(&face(Membership::Off, Kind::Show, true))));
    assert_eq!(add.label(ItemRow::WatchlistAdd), "Add Show to Watchlist");
    let on = menu(build_episode("9", PosterMark::None, &[], Some(&face(Membership::On, Kind::Show, true))));
    assert_eq!(on.label(ItemRow::WatchlistRemove), plx_platform::i18n::msg::watchlist_action_remove_show());
    assert_eq!(on.action(ItemRow::WatchlistRemove), Action::WatchlistRemove(guid_of(Kind::Show, &HEX)));
    let ep_card = card(3, Some(&face(Membership::Off, Kind::Show, true)));
    assert_eq!(ep_card.label(ItemRow::WatchlistAdd), "Add Show to Watchlist");
    let season = menu(build_season("9", PosterMark::None, Some(&face(Membership::Off, Kind::Show, true))));
    assert!(season.offers(ItemRow::WatchlistAdd));
}

#[test]
fn remove_is_destructive_and_add_is_not() {
    let on = card(0, Some(&face(Membership::On, Kind::Movie, false)));
    assert!(on.destructive(ItemRow::WatchlistRemove));
    let off = card(0, Some(&face(Membership::Off, Kind::Movie, false)));
    assert!(!off.destructive(ItemRow::WatchlistAdd));
    assert_eq!(on.ids().last(), Some(&ItemRow::WatchlistRemove), "the destructive row is last");
}

#[test]
fn a_watchlist_tile_card_keeps_its_one_remove_row() {
    let f = face(Membership::On, Kind::Movie, false);
    let m = menu(build_card(&movie(0), false, DeckPress::Play, None, Some("plex://movie/abc"), Some(&f)));
    assert_eq!(m.ids().iter().filter(|i| **i == ItemRow::WatchlistRemove).count(), 1);
}

const GUID: &str = "plex://movie/abababababababababababab";

fn catalog(state: TileState, member: Membership) -> Menu {
    menu(build_catalog(state, Kind::Movie, "", GUID, member))
}

#[test]
fn the_catalog_menu_rows_by_state_and_membership() {
    for state in [TileState::ComingSoon, TileState::NotOnAnyServer, TileState::ServerOffline, TileState::Unknown] {
        let on = catalog(state, Membership::On);
        assert_eq!(on.ids(), [ItemRow::GoToCatalog, ItemRow::WatchlistRemove], "{state:?}");
        assert_eq!(on.action(ItemRow::GoToCatalog), Action::GoToCatalog(GUID.into()));
        assert_eq!(on.action(ItemRow::WatchlistRemove), Action::WatchlistRemove(GUID.into()));
        assert!(on.destructive(ItemRow::WatchlistRemove));
        for member in [Membership::Off, Membership::Unknown] {
            let m = catalog(state, member);
            assert_eq!(m.ids(), [ItemRow::GoToCatalog, ItemRow::WatchlistAdd], "{state:?} {member:?}");
            assert_eq!(m.action(ItemRow::WatchlistAdd), Action::WatchlistAdd { hex: HEX });
            assert!(!m.destructive(ItemRow::WatchlistAdd));
        }
    }
    let matched = catalog(TileState::Available, Membership::On);
    assert_eq!(matched.ids(), [ItemRow::GoToItem, ItemRow::WatchlistRemove]);
}

#[test]
fn the_catalog_menu_opens_on_the_go_row() {
    for member in [Membership::On, Membership::Off, Membership::Unknown] {
        let m = catalog(TileState::ComingSoon, member);
        assert_eq!(m.0.opening_key(), Some(ItemRow::GoToCatalog.key()), "{member:?}");
    }
}

#[test]
fn identity_is_in_the_action_not_read_at_press_time() {
    // The rows are plain data built from the face: the guid / hex they carry are the ones
    // captured, and nothing is looked up when one is pressed.
    let m = card(0, Some(&face(Membership::Off, Kind::Movie, false)));
    assert_eq!(m.action(ItemRow::WatchlistAdd), Action::WatchlistAdd { hex: HEX });
    assert_eq!(hex_of(GUID), Some(HEX));
    assert_eq!(hex_of("plex://movie/nothex"), None);
    assert_eq!(guid_of(Kind::Show, &HEX), "plex://show/abababababababababababab");
}

#[test]
fn a_catalog_guid_without_an_id_offers_no_add() {
    let m = menu(build_catalog(TileState::ComingSoon, Kind::Movie, "", "g", Membership::Off));
    assert_eq!(m.ids(), [ItemRow::GoToCatalog]);
}

#[test]
fn watchlist_add_is_a_real_target_for_the_menu_guard() {
    assert!(Action::WatchlistAdd { hex: HEX }.has_subject());
    assert!(!Action::GoToItem(String::new()).has_subject());
    assert!(Action::GoToItem("1".into()).has_subject());
}
