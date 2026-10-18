//! The catalog id a Watchlist write needs, from a catalog row (`PmsMovie`): decoded once where the
//! row is built, and — when the row carries none — read off the item's own server record the way
//! `viewstate::resolved_guid` does (`client.metadata(rk or show_rk).guid`). No server is asked: the
//! "client" is a closure that hands back a parsed `Metadata`, exactly what `client.metadata` returns.

use plx_plex::plex::{catalog_hex_of_guid, resolve_catalog_hex, Envelope, Metadata};

use crate::watchlist::{decode_hex, encode_hex};

const HEX: &str = "0123456789abcdef01234567";

fn record(json: &str) -> Metadata {
    let body = format!(r#"{{"MediaContainer":{{"Metadata":[{json}]}}}}"#);
    let mut mc = serde_json::from_slice::<Envelope>(body.as_bytes()).expect("parse").media_container;
    mc.metadata.remove(0)
}

/// The plex-crate decoder and the store's `decode_hex` agree on the bytes, so a hex decoded at row
/// build time is the key the ledger and the write worker use.
#[test]
fn the_row_decoder_agrees_with_the_store_decoder() {
    let bytes = catalog_hex_of_guid(&format!("plex://movie/{HEX}")).expect("decodes");
    assert_eq!(Some(bytes), decode_hex(HEX));
    assert_eq!(encode_hex(&bytes), HEX);
}

#[test]
fn a_movie_with_no_guid_on_its_row_resolves_through_its_own_record() {
    let mut asked = Vec::new();
    let got = resolve_catalog_hex(None, || {
        asked.push("movie rk=11");
        record(&format!(r#"{{"ratingKey":"11","type":"movie","guid":"plex://movie/{HEX}"}}"#)).guid.into()
    });
    assert_eq!(got, decode_hex(HEX));
    assert_eq!(asked, ["movie rk=11"], "exactly one record read");
}

#[test]
fn an_episode_with_no_show_guid_resolves_through_the_shows_record() {
    // The show's record (`show_rk`), not the episode's: an episode's own guid is `plex://episode/…`.
    let show = record(&format!(r#"{{"ratingKey":"7","type":"show","guid":"plex://show/{HEX}"}}"#));
    assert_eq!(resolve_catalog_hex(None, || Some(show.guid.clone())), decode_hex(HEX));
    let episode = record(r#"{"ratingKey":"8","type":"episode","guid":"plex://episode/5fbd929d55d986002d63c021"}"#);
    assert_eq!(resolve_catalog_hex(None, || Some(episode.guid.clone())), None, "an episode guid names no show");
}

#[test]
fn a_carried_hex_costs_no_record_read_and_an_unanswered_read_leaves_none() {
    let carried = decode_hex(HEX);
    assert_eq!(resolve_catalog_hex(carried, || panic!("the row already knew")), carried);
    assert_eq!(resolve_catalog_hex(None, || None), None, "server did not answer: no write, a log line");
    let bare = record(r#"{"ratingKey":"12","type":"movie"}"#);
    assert_eq!(resolve_catalog_hex(None, || Some(bare.guid)), None, "no portable id: no row, no disc");
}

/// A season row carries its show's guid as `parentGuid` (the server sends no `grandparentGuid` on a
/// season), so the row knows its show without the record read; an episode keeps `grandparentGuid`.
#[test]
fn a_season_row_takes_its_show_guid_from_parent_guid() {
    let season = record(&format!(
        r#"{{"ratingKey":"21","type":"season","guid":"plex://season/5d7768264de0ee001fcc87e4","parentGuid":"plex://show/{HEX}"}}"#
    ));
    let row = crate::pms::parse_item(&season, plx_plex::plex::ServerId::UNSET);
    assert_eq!(row.show_guid, decode_hex(HEX));
    let (_, known, from_episode) = row.watchlist_subject().expect("a season acts on its show");
    assert!(from_episode);
    assert_eq!(resolve_catalog_hex(known, || panic!("no round trip")), decode_hex(HEX));
    let episode = record(&format!(
        r#"{{"ratingKey":"22","type":"episode","parentGuid":"plex://season/5d7768264de0ee001fcc87e4","grandparentGuid":"plex://show/{HEX}"}}"#
    ));
    let row = crate::pms::parse_item(&episode, plx_plex::plex::ServerId::UNSET);
    assert_eq!(row.show_guid, decode_hex(HEX), "an episode keeps grandparentGuid");
}
