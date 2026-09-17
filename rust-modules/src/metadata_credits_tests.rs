//! Cast & crew credit folding and the shelf's flat index space.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;

// ---- credits (Cast & Crew) ----------------------------------------------------------------

/// The crew fold, parsed from the shape PMS actually sends (verified live 2026-07-29): the
/// `Director[]`/`Writer[]` rows are `Role[]` rows MINUS the `role` attribute, so the job — the
/// only thing left to caption a crew tile with — exists nowhere but the array name.
///
/// Deliberately driven through serde rather than a hand-built `Metadata`, because the parse is
/// half the claim: if the DTO ever stops carrying `Director[]`, the tiles vanish silently.
#[test]
fn crew_credits_fold_both_job_arrays_into_one_deduplicated_shelf_list() {
    let body = br#"{
        "Director": [
            { "id": 161, "filter": "director=161", "tag": "Jane Doe",
              "tagKey": "5d77682a", "count": 3,
              "thumb": "https://metadata-static.plex.tv/c/people/c.jpg" },
            { "id": 162, "filter": "director=162", "tag": "" }
        ],
        "Writer": [
            { "id": 163, "filter": "writer=163", "tag": "Jane Doe",
              "tagKey": "5d77682a", "count": 3,
              "thumb": "https://metadata-static.plex.tv/c/people/c.jpg" },
            { "id": 164, "filter": "writer=164", "tag": "Sam Scribe" }
        ]
    }"#;
    let it: crate::plex::Metadata =
        serde_json::from_slice(body).expect("the live crew shape parses");
    assert_eq!(it.director.len(), 2, "Director[] is on the DTO");
    assert_eq!(it.writer.len(), 2, "and so is Writer[]");
    assert!(
        it.director[0].role.is_empty(),
        "crew rows carry no role — the JOB is the caption"
    );

    let crew = crew_credits(&it);
    let got: Vec<(&str, &str)> = crew
        .iter()
        .map(|c| (c.tag.as_str(), c.role.as_str()))
        .collect();
    assert_eq!(
        got,
        [("Jane Doe", "Director, Writer"), ("Sam Scribe", "Writer")],
        "directors first, and the writer-director is ONE tile listing both jobs — not two \
         identical headshots side by side"
    );
    assert_eq!(
        crew[0].thumb, "https://metadata-static.plex.tv/c/people/c.jpg",
        "the headshot rides along"
    );
    assert!(
        crew[1].thumb.is_empty(),
        "a crew member with no headshot is still a credit"
    );
}

/// The shelf's flat index space: the screen addresses one row of tiles, so `credit(i)` must run
/// the actors out first and then the crew, and refuse an index past the end rather than panic —
/// the focus column outlives the item it was set on (a Related jump reloads underneath it).
#[test]
fn the_credit_index_space_runs_every_actor_then_every_crew_member() {
    let person = |t: &str, r: &str| Cast {
        tag: t.to_string(),
        role: r.to_string(),
        thumb: String::new(),
        id: 0,
        tag_key: String::new(),
    };
    let d = Detail {
        cast: vec![person("Actor A", "Hero"), person("Actor B", "Villain")],
        crew: vec![person("Jane Doe", "Director")],
        ..Default::default()
    };
    assert_eq!(
        d.credits_len(),
        3,
        "the shelf is as long as the two lists together"
    );
    let seen: Vec<(&str, &str)> = (0..d.credits_len())
        .filter_map(|i| d.credit(i))
        .map(|c| (c.tag.as_str(), c.role.as_str()))
        .collect();
    assert_eq!(
        seen,
        [
            ("Actor A", "Hero"),
            ("Actor B", "Villain"),
            ("Jane Doe", "Director")
        ]
    );
    assert!(
        d.credit(3).is_none(),
        "one past the end is None, not a panic"
    );
    assert!(
        d.credit(usize::MAX).is_none(),
        "and so is a wildly stale focus column"
    );

    let crew_only = Detail {
        crew: vec![person("Jane Doe", "Director")],
        ..Default::default()
    };
    assert_eq!(
        crew_only.credits_len(),
        1,
        "a crew-only item still fills the shelf"
    );
    assert_eq!(
        crew_only.credit(0).map(|c| c.tag.as_str()),
        Some("Jane Doe"),
        "and its first tile is the crew"
    );
}
