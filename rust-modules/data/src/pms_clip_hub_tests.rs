//! Clip hubs on Home (#599). Plex groups an Other Videos library's recently-added row as a hub of
//! type `clip` (`home.videos.recent`), whose ITEMS are `movie` rows of subtype `clip` (measured on
//! PMS 1.43.5). The hub's type is the server's grouping word; what makes a card is the item.

use super::*;
#[allow(unused_imports)]
use super::test_support::*;

fn container(body: &str) -> plx_plex::plex::MediaContainer {
    serde_json::from_str::<plx_plex::plex::Envelope>(body).expect("the hub list parses").media_container
}

/// Project one `/hubs` answer and merge it as the only source.
fn home_of(body: &str) -> (Vec<Arc<PmsMovie>>, Vec<HubRow>, Vec<HeroSlot>) {
    let build = project(&container(body), &plx_plex::plex::MediaContainer::default(), sid(0));
    merge(&[src(0, "", HubState::Ready, Some(build))])
}

/// A clip hub of two `movie/clip` rows of library 3, with poster and backdrop.
fn clip_hub(id: &str, key: &str) -> String {
    format!(r#"{{"type":"clip","hubIdentifier":"{id}","title":"{id}","key":"{key}",
        "Metadata":[
          {{"ratingKey":"901","librarySectionID":"3","librarySectionTitle":"Other Videos","type":"movie",
            "subtype":"clip","title":"Clip A","thumb":"/a.jpg","art":"/a-art.jpg"}},
          {{"ratingKey":"902","librarySectionID":"3","librarySectionTitle":"Other Videos","type":"movie",
            "subtype":"clip","title":"Clip B","thumb":"/b.jpg","art":"/b-art.jpg"}}]}}"#)
}

const MOVIES_HUB: &str = r#"{"type":"movie","hubIdentifier":"home.movies.recent","title":"Recently Added Movies",
    "key":"/hubs/home/recentlyAdded?type=1","Metadata":[
      {"ratingKey":"1","librarySectionID":"1","librarySectionTitle":"Movies","type":"movie","title":"A Film",
       "thumb":"/m.jpg","art":"/m-art.jpg"}]}"#;

fn answer(hubs: &[String]) -> String {
    format!(r#"{{"MediaContainer":{{"Hub":[{}]}}}}"#, hubs.join(","))
}

/// **A clip hub whose items are movies is a Home row.** The hub gate used to read the hub's type
/// (mixed, or a listable one) and dropped it, so a library the server shows on Home (`hidden` 0)
/// had no row there. The title is the catalogue's, from the hub's id.
#[test]
fn a_clip_hub_whose_items_are_movies_is_a_home_row() {
    let (items, hubs, _) = home_of(&answer(&[clip_hub("home.videos.recent", "/hubs/home/recentlyAdded?type=1&personal=1")]));
    assert_eq!(hubs.len(), 1, "the clip hub reaches Home");
    assert_eq!(hubs[0].title, "Recently Added Videos");
    assert_eq!(hubs[0].hub_id, "home.videos.recent");
    assert_eq!(items.iter().map(|m| (m.rk.as_str(), m.sec)).collect::<Vec<_>>(), [("901", 3), ("902", 3)]);
}

/// A clip-typed ITEM is an extra (a trailer, a behind-the-scenes), not a card: the item rule stays
/// as it was, so a hub made of them is still no row.
#[test]
fn a_clip_typed_extra_is_still_not_a_card() {
    let extras = r#"{"type":"clip","hubIdentifier":"home.videos.recent","title":"Recently Added Videos",
        "key":"/hubs/home/recentlyAdded?type=1&personal=1","Metadata":[
          {"ratingKey":"7","librarySectionID":"3","type":"clip","subtype":"trailer","title":"Trailer",
           "thumb":"/t.jpg","art":"/t-art.jpg"}]}"#;
    let (items, hubs, _) = home_of(&answer(&[extras.to_string()]));
    assert!(hubs.is_empty() && items.is_empty());
    assert!(shelf_card(&container(&answer(&[extras.to_string()])).hub[0].metadata[0], sid(0)).is_none());
}

/// Hubs of any other type stay out: the server's music and photos hubs sit in the same list, with
/// size 0 or with items this product has no level for.
#[test]
fn a_hub_of_an_unlisted_type_is_still_skipped() {
    let photos = r#"{"type":"photo","hubIdentifier":"home.photos.recent","title":"Recently Added Photos",
        "key":"/hubs/home/recentlyAdded?type=13","Metadata":[
          {"ratingKey":"8","type":"photo","title":"Snap","thumb":"/p.jpg","art":"/p-art.jpg"}]}"#;
    let album = r#"{"type":"album","hubIdentifier":"home.music.recent","title":"Recently Added Music",
        "key":"/hubs/home/recentlyAdded?type=9","Metadata":[
          {"ratingKey":"9","type":"album","title":"Record","thumb":"/r.jpg","art":"/r-art.jpg"}]}"#;
    let (_, hubs, _) = home_of(&answer(&[photos.to_string(), album.to_string(), MOVIES_HUB.to_string()]));
    assert_eq!(hubs.iter().map(|h| h.hub_id.as_str()).collect::<Vec<_>>(), ["home.movies.recent"]);
}

/// **A recently added personal clip never feeds the hero pool**, in either family of ids (an in-progress
/// one still reaches it through Continue Watching, the deck): the whole-server
/// `home.videos.recent` and a section's own `video.recent.<id>` (which the title table admits in Home
/// scope). The pool takes the movie row beside them, which is the control.
#[test]
fn other_videos_never_feed_the_hero() {
    let body = answer(&[
        clip_hub("home.videos.recent", "/hubs/home/recentlyAdded?type=1&personal=1"),
        clip_hub("video.recent.3", "/library/sections/3/all?sort=addedAt:desc"),
        MOVIES_HUB.to_string(),
    ]);
    let (items, hubs, pool) = home_of(&body);
    assert_eq!(hubs.len(), 3, "all three are rows");
    let pooled: Vec<&str> = pool.iter().map(|slot| items[slot.idx].rk.as_str()).collect();
    assert_eq!(pooled, ["1"], "only the movie row feeds the hero: {pooled:?}");
}

/// The same exclusion where a refresh spends the hero's card allowance on rows it would otherwise
/// give up (`Projection::keeps`): a clip row must not use it up, the movie row does.
#[test]
fn a_refresh_spends_the_hero_allowance_on_recent_movies_and_never_on_clips() {
    let none: &[(String, String)] = &[];
    let mut clips = Projection::new(sid(0), Some(none));
    clips.keeps(&shelf(0, "", "home.videos.recent", &["1", "2"]));
    clips.keeps(&shelf(0, "", "video.recent.3", &["3"]));
    assert_eq!(clips.hero_left, HERO_MAX, "clip rows spend none of the allowance");
    let mut movies = Projection::new(sid(0), Some(none));
    movies.keeps(&shelf(0, "", "home.movies.recent", &["1", "2"]));
    assert_eq!(movies.hero_left, HERO_MAX - 2);
}

/// Unpinning the Other Videos library in Favorite libraries takes its Home row away: the row's cards
/// carry the library's section id and the pin filter reads it, as for any other library.
#[test]
fn an_unpinned_library_keeps_its_clip_row_off_home() {
    let (items, hubs, _) = home_of(&answer(&[clip_hub("home.videos.recent", "/hubs/home/recentlyAdded?type=1&personal=1")]));
    assert_eq!(hubs.len(), 1);
    let off = [(sid(0), 3, false)];
    let on = [(sid(0), 3, true)];
    assert!(items.iter().all(|m| !item_pinned(&off, m)), "switched off: no card of the row passes");
    assert!(items.iter().all(|m| item_pinned(&on, m)));
    assert!(items.iter().all(|m| item_pinned(&[(sid(0), 1, false)], m)), "another library's pin is not this one's");
}

/// **An Other Videos library the user hid publishes no clip card, on the shelf the server filed it
/// under.** The shelf survives in the build (it is the server's row); what Home publishes, and
/// whether the shelf takes a place in the order at all, is decided by the library's switch.
#[test]
fn a_hidden_other_videos_library_publishes_no_clip_row() {
    let sh = shelf_in(0, 3, "", "home.videos.recent", &["901", "902"]);
    let srcs = [src(0, "", HubState::Ready, Some(built(0, &[], vec![sh.clone()])))];
    let off = [(sid(0), 3, false), (sid(0), 1, true)];
    let on = [(sid(0), 3, true), (sid(0), 1, true)];
    assert_eq!(publishable(&sh, &off).count(), 0, "switched off: no card");
    assert!(published_order(&srcs, &off).is_empty(), "…and no place in the order");
    assert_eq!(publishable(&sh, &on).count(), 2, "switched on: both cards");
    assert_eq!(published_order(&srcs, &on), [(0, 0)]);
}

/// **A personal-video row whose cards name no library cannot prove its library is a favourite.**
/// `item_pinned` lets a card with no `librarySectionID` through ("unknown is allowed"), which is
/// right for a movie row and would publish a clip from a library that is off. Insurance: every
/// item list measured on a real server carries the id, the global clip hub among them being empty
/// there (`hidden` 1).
#[test]
fn a_clip_row_that_names_no_library_is_not_published() {
    let clips = shelf(0, "", "home.videos.recent", &["901", "902"]);
    let section_clips = shelf(0, "", "video.recent.3", &["903"]);
    let movies = shelf(0, "", "home.movies.recent", &["1"]);
    let pins = [(sid(0), 3, false), (sid(0), 1, true)];
    assert_eq!(publishable(&clips, &pins).count(), 0, "home.videos.* without a section id");
    assert_eq!(publishable(&section_clips, &pins).count(), 0, "video.* without a section id");
    assert_eq!(publishable(&movies, &pins).count(), 1, "a movie row keeps the unknown-is-allowed rule");
    let srcs = [src(0, "", HubState::Ready, Some(built(0, &[], vec![clips, section_clips, movies])))];
    assert_eq!(published_order(&srcs, &pins), [(0, 2)], "only the movie shelf takes a place");
}
