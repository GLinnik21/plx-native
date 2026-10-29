//! The bookmark disc: where it sits in the row, which face it wears, how a flip moves, and what a
//! press asks for. Pure values only — the paint and the effect push are `DetailScreen`'s.

use super::*;
use plx_data::catalog::Membership;

const HEX: &str = "5d776b1d96b655001f2a9c3e";
const GUID: &str = "plex://movie/5d776b1d96b655001f2a9c3e";

fn sets() -> Vec<HeroSet> {
    let mut v = Vec::new();
    for mark in [PosterMark::None, PosterMark::InProgress, PosterMark::Watched] {
        for version in [false, true] {
            for restart in [false, true] {
                for trailer in [false, true] {
                    for alt in [false, true] {
                        v.push(HeroSet { watchlist: true, version, restart, trailer, alt, mark });
                    }
                }
            }
        }
    }
    v
}

/// The disc is SECOND in every row the hero can take, and the rest keep their relative order.
#[test]
fn the_disc_is_second_after_play_in_every_row() {
    for s in sets() {
        let (v, n) = hero_ctls(s);
        assert_eq!(v[0], HeroCtl::Play, "{s:?}");
        assert_eq!(v[1], HeroCtl::Watchlist, "{s:?}");
        assert_eq!(v[..n].iter().filter(|c| **c == HeroCtl::Watchlist).count(), 1, "{s:?}");
        let rest: Vec<_> = v[2..n].to_vec();
        let mut want = Vec::new();
        if s.restart { want.push(HeroCtl::Restart); }
        if s.trailer { want.push(HeroCtl::Trailer); }
        if s.alt { want.push(HeroCtl::Alt); }
        want.push(if s.mark == PosterMark::Watched { HeroCtl::MarkUnwatched } else { HeroCtl::MarkWatched });
        if s.version { want.push(HeroCtl::Version); }
        assert_eq!(rest, want, "{s:?}");
        assert!(n <= MAX_CTLS);
    }
}

/// Focus moves by index: Play to the disc to whatever follows, and back; the identity survives a
/// membership flip because it is one elem for both faces.
#[test]
fn focus_walks_play_then_disc_then_the_rest() {
    let s = HeroSet { watchlist: true, version: false, restart: true, trailer: false, alt: false, mark: PosterMark::None };
    assert_eq!(ctl_at(s, 0), Some(HeroCtl::Play));
    assert_eq!(ctl_at(s, 1), Some(HeroCtl::Watchlist));
    assert_eq!(ctl_at(s, 2), Some(HeroCtl::Restart));
    assert_eq!(index_of(s, HeroCtl::Watchlist), Some(1));
    assert_eq!(index_of(s, HeroCtl::Restart), Some(2));
    assert_eq!(watch_index(s), Some(3));
}

#[test]
fn the_elem_id_is_fresh_in_the_hero_range_and_round_trips() {
    let all = [
        ELEM_PLAY, ELEM_RESTART, ELEM_ALT, ELEM_MARK_WATCHED, ELEM_MARK_UNWATCHED, ELEM_TRAILER,
        ELEM_WATCHLIST, ELEM_VERSION,
    ];
    let mut seen = std::collections::BTreeSet::new();
    for e in all {
        assert!(e < HERO_ELEM_RANGE_END);
        assert!(seen.insert(e), "elem {e} is minted twice");
        assert_eq!(HeroCtl::of_elem(e).map(HeroCtl::elem), Some(e));
    }
    assert_eq!(HeroCtl::Watchlist.elem(), ELEM_WATCHLIST);
    assert_eq!(HeroCtl::of_elem(HERO_ELEM_RANGE_END), None);
}

/// Full-trailer mode still narrows the row to Play alone: the disc is not an anchor.
#[test]
fn full_trailer_mode_drops_the_disc_from_the_focus_extent() {
    for s in sets() {
        let (v, n) = visible_ctls(s, true);
        assert_eq!((v[0], n), (HeroCtl::Play, 1), "{s:?}");
        assert!(!focusable(HeroCtl::Watchlist, true));
        assert!(focusable(HeroCtl::Watchlist, false));
        assert_eq!(visible_ctls(s, false).1, hero_ctls(s).1);
    }
}

/// No labels: no verb, no unfurl slot, and a circle width however far the other discs unfurl.
#[test]
fn the_disc_has_no_label_and_never_widens() {
    for named in [false, true] {
        assert!(disc_verb(HeroCtl::Watchlist, named).is_none());
    }
    let s = HeroSet { watchlist: true, version: false, restart: true, trailer: true, alt: true, mark: PosterMark::None };
    let widths = HeroWidths { pill: 236.0, alt: 300.0, disc: [CD + 90.0, CD + 60.0, CD + 120.0, CD + 70.0] };
    let r = hero_btn_rect_at(s, 1, 0.0, widths);
    assert_eq!((r.w, r.h), (CD, CD));
    let play = hero_btn_rect_at(s, 0, 0.0, widths);
    assert_eq!(r.x, play.x + play.w + CGAP, "the disc sits one gap after Play");
    let next = hero_btn_rect_at(s, 2, 0.0, widths);
    assert_eq!(next.x, r.x + r.w + CGAP);
}

/// Membership x press: Add from Off and Unknown, Remove from On; no usable guid, no press.
#[test]
fn the_press_table() {
    use crate::registry::ItemMenuAction as Action;
    let hex = plx_data::watchlist::decode_hex(HEX).unwrap();
    assert_eq!(guid_hex(GUID), Some(hex));
    assert_eq!(press(GUID, Membership::Off), Some(Action::WatchlistAdd { hex }));
    assert_eq!(press(GUID, Membership::Unknown), Some(Action::WatchlistAdd { hex }));
    assert_eq!(press(GUID, Membership::On), Some(Action::WatchlistRemove(GUID.into())));
    for bad in ["", "plex://movie/", "plex://movie/zz", "/library/metadata/42"] {
        assert_eq!(press(bad, Membership::Off), None, "{bad:?}");
        assert_eq!(press(bad, Membership::On), None, "{bad:?}");
    }
}

/// Without a Watchlist-takeable guid the row has no disc at all: Play, then whatever follows.
#[test]
fn an_item_without_a_watchlist_guid_has_no_disc() {
    for s in sets() {
        let s = HeroSet { watchlist: false, ..s };
        let (v, n) = hero_ctls(s);
        assert!(!v[..n].contains(&HeroCtl::Watchlist), "{s:?}");
        assert_eq!(v[0], HeroCtl::Play);
        assert_eq!(index_of(s, HeroCtl::Watchlist), None);
        assert!(!visible_ctls(s, false).0[..visible_ctls(s, false).1].contains(&HeroCtl::Watchlist));
    }
}
