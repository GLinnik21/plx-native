//! The tails of a result row (`search/tail.rs`): a lane read window by window against a served fake.

use super::tail::{Io, Lane, TvMap};
use super::*;
use plx_plex::plex::{MediaContainer, Metadata, PageReq, SearchKind, SearchResult, Tag};

fn sid() -> ServerId { ServerId::from_raw(0) }

fn md(ty: &str, rk: usize) -> Metadata {
    Metadata { kind: ty.into(), rating_key: rk.to_string(), title: format!("{ty}{rk}"), ..Default::default() }
}

fn person(n: usize) -> Tag { Tag { tag: format!("p{n}"), tag_key: format!("k{n}"), id: n as i64, ..Default::default() } }

/// A server's lists for one query, and every request made of it.
#[derive(Default)]
pub(super) struct Fake {
    pub movies: Vec<(&'static str, usize)>,
    pub tv: Vec<(&'static str, usize)>,
    pub people: Vec<usize>,
    pub collections: Vec<(&'static str, usize)>,
    pub typed: bool,
    pub log: Vec<(SearchKind, usize, usize)>,
    pub hubs_asked: Vec<usize>,
}

impl Fake {
    pub fn new() -> Fake { Fake { typed: true, ..Default::default() } }
    pub fn movies(mut self, n: usize) -> Fake { self.movies = (0..n).map(|i| ("movie", i)).collect(); self }
    pub fn tv(mut self, shows: usize, episodes: usize) -> Fake {
        self.tv = (0..shows).map(|i| ("show", i)).chain((0..episodes).map(|i| ("episode", 1000 + i))).collect();
        self
    }
    fn of(&self, ty: &str) -> Vec<(&'static str, usize)> { self.tv.iter().filter(|m| m.0 == ty).copied().collect() }
}

fn card(m: &(&str, usize)) -> Item { Item::Media(crate::pms::parse_item(&md(m.0, m.1), sid())) }

impl Io for Fake {
    fn listing(&mut self, kind: SearchKind, req: PageReq) -> Option<MediaContainer> {
        self.log.push((kind, req.start, req.size));
        let mut mc = MediaContainer { offset: req.start as i64, ..Default::default() };
        if !self.typed { return Some(mc); }
        let rows: Vec<SearchResult> = match kind {
            SearchKind::Movies => self.movies.iter().map(|m| SearchResult { metadata: Some(md(m.0, m.1)), directory: None }).collect(),
            SearchKind::Tv => self.tv.iter().map(|m| SearchResult { metadata: Some(md(m.0, m.1)), directory: None }).collect(),
            SearchKind::People => self.people.iter().map(|n| SearchResult { metadata: None, directory: Some(person(*n)) }).collect(),
        };
        mc.search_result = rows.into_iter().skip(req.start).take(req.size).collect();
        Some(mc)
    }
    fn hubs(&mut self, limit: usize) -> Option<Projection> {
        self.hubs_asked.push(limit);
        let mut out: Projection = Default::default();
        out[0] = self.movies.iter().take(limit).map(card).collect();
        out[1] = self.of("show").iter().take(limit).map(card).collect();
        out[2] = self.of("episode").iter().take(limit).map(card).collect();
        out[3] = self.people.iter().take(limit).map(|n| Item::Tag(tag_hit(&person(*n), sid(), &[]))).collect();
        out[4] = self.collections.iter().take(limit).map(|m| Item::Collection(CollectionHit::from_row(&md(m.0, m.1), sid()))).collect();
        Some(out)
    }
}

pub(super) fn preview(fake: &Fake, kind: Kind) -> Vec<Item> {
    let mut p = Fake { typed: true, movies: fake.movies.clone(), tv: fake.tv.clone(), people: fake.people.clone(),
        collections: fake.collections.clone(), ..Default::default() };
    let k = KINDS.iter().position(|x| *x == kind).unwrap();
    p.hubs(12).unwrap()[k].clone()
}

fn key(it: &Item) -> String { tail::key_of(it) }

/// Walk a lane forward the way the store does: each move reads half a window further and keeps `span`
/// depths, so the lane never holds more than a window and a page. Returns the depths seen in order.
fn walk(lane: &mut Lane, kind: Kind, tv: &mut TvMap, io: &mut Fake, span: usize) -> Vec<String> {
    let (mut lo, mut seen, mut next) = (0, Vec::new(), 0);
    loop {
        let hi = lo + span;
        lane.read(kind, tv, lo..hi, io, sid()).unwrap();
        // a sampled lane has holes where the listing repeated a preview card
        for d in next..hi { if let Some(it) = lane.get(d) { seen.push(key(it)); } }
        next = hi;
        assert!(lane.held() <= span + tail::PAGE, "lane holds {} rows", lane.held());
        if !lane.has_past(hi) { return seen; }
        lo += span / 2;
        lane.trim(lo..lo + span);
    }
}

#[test]
fn three_hundred_movies_are_reached_to_the_last_and_back_within_a_window() {
    let mut fake = Fake::new().movies(300);
    let mut lane = Lane::from_preview(Kind::Movie, &preview(&fake, Kind::Movie));
    let mut tv = TvMap::default();
    let seen = walk(&mut lane, Kind::Movie, &mut tv, &mut fake, 24);
    let want: Vec<String> = (0..300).map(|i| i.to_string()).collect();
    assert_eq!(seen, want, "every hit, once, in order");
    assert_eq!(lane.end, Some(300), "the end is a short page");
    for lo in (0..276).rev().step_by(12) {
        lane.read(Kind::Movie, &mut tv, lo..lo + 24, &mut fake, sid()).unwrap();
        lane.trim(lo..lo + 24);
        assert!((lo..lo + 24).all(|d| lane.get(d).map(key) == Some(d.to_string())), "window at {lo}");
        assert!(lane.held() <= 24);
    }
}

#[test]
fn the_show_count_is_the_short_preview_hub() {
    let fake = Fake::new().tv(5, 40);
    let tv = TvMap::from_previews(preview(&fake, Kind::Show).len());
    assert_eq!(tv.first_episode, Some(5));
    assert_eq!(TvMap::from_previews(12).first_episode, None, "a full hub says nothing");
}

#[test]
fn the_show_row_learns_where_the_episodes_start_and_the_episode_row_reads_from_there() {
    let mut fake = Fake::new().tv(40, 60);
    let mut tv = TvMap::from_previews(12);
    let mut shows = Lane::from_preview(Kind::Show, &preview(&fake, Kind::Show));
    let seen = walk(&mut shows, Kind::Show, &mut tv, &mut fake, 24);
    assert_eq!(seen, (0..40).map(|i| i.to_string()).collect::<Vec<_>>(), "shows end where the episodes begin");
    assert_eq!(tv.first_episode, Some(40));
    fake.log.clear();
    let mut eps = Lane::from_preview(Kind::Episode, &preview(&fake, Kind::Episode));
    let seen = walk(&mut eps, Kind::Episode, &mut tv, &mut fake, 24);
    assert_eq!(seen, (0..60).map(|i| (1000 + i).to_string()).collect::<Vec<_>>());
    assert!(fake.log.iter().all(|(_, _, size)| *size > 1), "no probing once S is known: {:?}", fake.log);
}

#[test]
fn the_episode_row_scrolled_first_finds_the_start_in_a_logarithmic_number_of_requests() {
    for shows in [13usize, 40, 200, 1000] {
        let mut fake = Fake::new().tv(shows, 50);
        let mut tv = TvMap::from_previews(12);
        let mut eps = Lane::from_preview(Kind::Episode, &preview(&fake, Kind::Episode));
        eps.read(Kind::Episode, &mut tv, 12..36, &mut fake, sid()).unwrap();
        assert_eq!(tv.first_episode, Some(shows));
        assert_eq!(eps.get(12).map(key), Some("1012".to_string()));
        let single = fake.log.iter().filter(|(_, _, size)| *size == 1).count();
        let bound = 2 * (usize::BITS - shows.leading_zeros()) as usize;
        assert!(single <= bound, "{shows} shows took {single} one-row requests (bound {bound})");
    }
}

#[test]
fn a_show_added_while_the_row_is_open_moves_nothing_twice() {
    let mut fake = Fake::new().tv(60, 5);
    let mut tv = TvMap::from_previews(12);
    let mut shows = Lane::from_preview(Kind::Show, &preview(&fake, Kind::Show));
    shows.read(Kind::Show, &mut tv, 0..36, &mut fake, sid()).unwrap();
    fake.tv.insert(0, ("show", 9000)); // every offset moves by one
    shows.read(Kind::Show, &mut tv, 24..70, &mut fake, sid()).unwrap();
    let got: Vec<String> = (24..70).filter_map(|d| shows.get(d).map(key)).collect();
    assert_eq!(got, (24..60).map(|i| i.to_string()).collect::<Vec<_>>(), "no repeat, no skip, and the shows end at the episodes");
    assert_eq!(tv.first_episode, Some(61), "S moved with the shows");
}

#[test]
fn people_are_a_sample_of_the_preview_and_no_card_repeats() {
    let mut fake = Fake::new();
    fake.people = (0..70).collect();
    let mut prev: Vec<Item> = (0..5).map(|n| Item::Tag(tag_hit(&person(n), sid(), &[]))).collect();
    prev.push(Item::Tag(tag_hit(&person(60), sid(), &[])));
    let mut lane = Lane::from_preview(Kind::Person, &prev);
    let mut tv = TvMap::default();
    let seen = walk(&mut lane, Kind::Person, &mut tv, &mut fake, 24);
    let mut sorted = seen.clone();
    sorted.sort(); sorted.dedup();
    assert_eq!(sorted.len(), seen.len(), "a preview card is never drawn twice");
    assert_eq!(seen.len(), 70, "every person of the listing and the preview, once");
}

#[test]
fn a_kind_the_server_does_not_list_is_reached_by_growing_the_limit() {
    let mut fake = Fake { typed: false, ..Fake::default() }.movies(300);
    let mut lane = Lane::from_preview(Kind::Movie, &preview(&fake, Kind::Movie));
    let mut tv = TvMap::default();
    let seen = walk(&mut lane, Kind::Movie, &mut tv, &mut fake, 24);
    assert_eq!(seen, (0..300).map(|i| i.to_string()).collect::<Vec<_>>());
    assert_eq!(lane.end, Some(300), "growth ends on a response shorter than its limit");
    assert!(!fake.hubs_asked.is_empty() && fake.hubs_asked.windows(2).all(|w| w[0] <= w[1]), "the limit only grows");
}

#[test]
fn collections_have_no_typed_listing_and_grow_to_the_end() {
    let mut fake = Fake::new();
    fake.collections = (0..50).map(|i| ("collection", 500 + i)).collect();
    let mut lane = Lane::from_preview(Kind::Collection, &preview(&fake, Kind::Collection));
    let mut tv = TvMap::default();
    let seen = walk(&mut lane, Kind::Collection, &mut tv, &mut fake, 24);
    assert_eq!(seen.len(), 50);
    assert!(fake.log.is_empty(), "no typed request is made for collections");
    assert_eq!(lane.end, Some(50));
}
