//! The demo library's two fixture titles, parsed from the mock's captured detail responses
//! (`tests/demo_library/fixtures/detail-<rk>.json`, written by `tools/demo_library.py fixtures`).
//!
//! `fetch_detail` builds a `Detail` inline from a network read, so these tests drive the same
//! pure converters it calls (`crew_credits`, `convert_ratings`, `dedup_tags`) on the parsed
//! `Metadata` instead of a whole `Detail`.

use super::*;

fn fixture(rk: u32) -> plx_plex::plex::Metadata {
    let text = match rk {
        102 => include_str!("../../../tests/demo_library/fixtures/detail-102.json"),
        105 => include_str!("../../../tests/demo_library/fixtures/detail-105.json"),
        _ => unreachable!("no fixture for {rk}"),
    };
    let mut body: serde_json::Value = serde_json::from_str(text).expect("the fixture is JSON");
    let row = body["MediaContainer"]["Metadata"][0].take();
    serde_json::from_value(row).expect("the mock's detail row parses like a server's")
}

fn names(tags: &[plx_plex::plex::Tag]) -> Vec<&str> {
    tags.iter().map(|t| t.tag.as_str()).collect()
}

#[test]
fn sintel_serves_every_field_the_detail_page_draws_that_the_catalog_supplies() {
    let it = fixture(102);
    assert_eq!(it.title, "Sintel");
    assert!(!it.tagline.is_empty(), "the About panel's tagline");
    assert_eq!(names(&it.country), ["Netherlands"]);
    assert_eq!(dedup_tags(&it.director), ["Colin Levy"]);
    assert_eq!(names(&it.writer), ["Esther Wouda", "Martin Lodewijk"]);
    assert_eq!(names(&it.role), ["Halina Reijn", "Thom Hoffman"]);
    assert_eq!(it.role[0].role, "Sintel");
    assert!(!it.genre.is_empty());
    let crew: Vec<_> = crew_credits(&it).into_iter().map(|c| c.tag).collect();
    assert_eq!(crew, ["Colin Levy", "Esther Wouda", "Martin Lodewijk"]);
}

#[test]
fn tears_of_steel_serves_its_full_cast_and_its_writer_director() {
    let it = fixture(105);
    assert_eq!(it.title, "Tears of Steel");
    assert!(!it.tagline.is_empty());
    assert_eq!(names(&it.country), ["Netherlands"]);
    assert_eq!(names(&it.role).len(), 7, "every credited performer");
    let crew = crew_credits(&it);
    assert_eq!(crew.len(), 1);
    assert_eq!(
        (crew[0].tag.as_str(), crew[0].role.as_str()),
        ("Ian Hubert", "Director, Writer"),
        "one tile for the writer-director"
    );
}

#[test]
fn neither_title_serves_a_score_or_a_certification() {
    // No cited source exists for either, so the page shows "Unrated" and no ratings row.
    for rk in [102, 105] {
        let it = fixture(rk);
        assert!(it.content_rating.is_empty(), "{rk}: no uncited certification");
        assert!(convert_ratings(&it).is_empty(), "{rk}: no uncited score");
    }
}

#[test]
fn a_headshot_is_served_only_for_a_person_the_catalog_gives_a_portrait() {
    // The catalog's `people` is the cast of Tears of Steel (an openly licensed photograph of the
    // actor, or a frame of the film): each of them carries a `thumb`, every other credit carries
    // none and the page draws its glyph.
    let sintel = fixture(102);
    for t in sintel.role.iter().chain(&sintel.director).chain(&sintel.writer) {
        assert!(t.thumb.is_empty(), "102: {} has no photograph", t.tag);
    }
    let tears = fixture(105);
    for t in &tears.role {
        assert!(t.thumb.starts_with("/library/metadata/"), "105: {} has a portrait", t.tag);
    }
    for t in tears.director.iter().chain(&tears.writer) {
        assert!(t.thumb.is_empty(), "105: {} has no photograph", t.tag);
    }
    assert_eq!(tears.role[3].role, "Thom", "Vanja Rukavina plays the young Thom");
    assert_eq!(tears.role[4].role, "Celia", "Denise Rebergen plays Celia");
}
