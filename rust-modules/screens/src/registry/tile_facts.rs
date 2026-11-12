//! The one place a catalog row (`pms::PmsMovie`) becomes the plain [`TileFacts`] `ui` draws a tile
//! from.
//!
//! `ui` is the library and names no application type (`docs/module-layers.md`, the `ui` layer), so
//! its poster and still tiles, its captions and its page wash take a value the caller fills rather
//! than the row. A screen is the lowest layer that may name both, so the conversion lives here, in
//! the registry every screen is allowed to reach, and not in `ui` (which cannot name the row) or on
//! `PmsMovie` (the data layer cannot name `ui`). Every rule that DECIDES something stays with the
//! layer that owns it and arrives already answered:
//!
//! - **what is in progress** is [`PmsMovie::resume_frac`], the one resume rule, applied here to fill
//!   [`TileFacts::resume`], so the bar, the watched mark and the Continue Watching caption cannot
//!   read it differently;
//! - **whether a thumb is the server's generated collection composite** is
//!   [`plx_plex::plex::collections::composite_parts`], answered here as `composite_thumb`;
//! - **what kind a row is** is the row's numeric `kind`, named here.
//!
//! Cheap by construction: every text field is a borrow of the row, so building one per tile per
//! frame allocates nothing. Callers build it where they use it (`Art::Poster(Some(of(item)))`)
//! and keep none.

use std::os::raw::c_int;

use plx_data::pms::{PmsMovie, KIND_COLLECTION};
use plx_ui::caption_policy::CaptionFacts;
use plx_plex::plex::artwork;
use plx_ui::tile::{DeckArt, Resume, TileFacts, TileKind};

/// The facts of a catalog row a caption policy words its lines from: every poster shelf's captions
/// (Home, Recommended, the Library grid, the Watchlist) and the Under posters preview's tiles read
/// one conversion, so a row is worded the same everywhere.
///
/// `title` is the name a reader knows the tile by, which is not always the row's own: a SEASON is
/// known by its SHOW (Plex's own clients caption a season tile with the show's name), and the
/// season's own title ("Season 6") becomes `season`, the line the details fall back to. A season
/// the server sent no show for keeps its own title.
pub fn caption_facts(item: &PmsMovie) -> CaptionFacts {
    let season = item.kind == 2;
    CaptionFacts {
        title: Some(if season && !item.show_title.is_empty() { item.show_title.clone() } else { item.title.clone() }),
        season: season.then(|| (u32::try_from(item.season_index).unwrap_or(0), item.title.clone())),
        year: Some(item.year),
        duration_min: u32::try_from(item.dur_ns / 60_000_000_000).ok(),
        genre: item.genre.clone(),
        video_resolution: item.video_resolution.clone(),
        hdr: item.hdr,
        content_rating: Some(item.rating.clone()),
        audience_rating: item.audience_rating,
        leaf_count: item.leaf_count,
        viewed_leaf_count: item.viewed_leaf_count,
        child_count: u32::try_from(item.child_count).ok().filter(|n| *n > 0),
        subtype: (item.kind == KIND_COLLECTION).then(|| item.subtype.clone().unwrap_or_default()),
        is_show: item.kind == 1,
        is_movie: item.kind == 0,
        episode: (item.kind == 3).then(|| (
            u32::try_from(item.season_index).unwrap_or(0),
            u32::try_from(item.ep_index).unwrap_or(0),
        )),
        show: (item.kind == 3 || season).then(|| item.show_title.clone()).filter(|show| !show.is_empty()),
        name_cut: item.kind == KIND_COLLECTION && face_cuts_name(item),
        ..Default::default()
    }
}

/// **Does this collection's face cut its name?** The face is the grid's resting card, drawn by the
/// ONE card art every poster surface shares (`widgets::card_named_led`: a baked fan names itself
/// live, a collection with no artwork wears the neutral tile's name, a custom poster is artwork with
/// no name on it to cut), so the answer is the row's, the same on the Collections page, Search, Home,
/// Recommended, Detail and the Collection screen. A name the face cuts is carried whole by the
/// caption ([`CaptionFacts::name_cut`]).
pub fn face_cuts_name(item: &PmsMovie) -> bool {
    use plx_ui::collection_tile::{name_is_cut, Face};
    let facts = of(item);
    let face = if facts.thumb.is_empty() { Face::Neutral } else if facts.composite_thumb { Face::Fan } else { return false };
    name_is_cut(face, &item.title, plx_ui::Rect::new(0.0, 0.0, plx_ui::consts::CARD_W, plx_ui::consts::CARD_H))
}

/// Progress and time left shared by the hero action and the tiles.
pub fn resume_of(m: &PmsMovie) -> Option<Resume> {
    m.resume_frac().map(|frac| Resume {
        frac,
        left_ms: m.dur_ns / 1_000_000 - m.resume_ms,
    })
}

/// What `ui` reads of `m`. The result borrows from the row for `'a`, and carries the row's address
/// as [`TileFacts::owner`], which is how a card's placement history stays keyed to the ROW and not
/// to the short-lived facts built from it.
pub fn of(m: &PmsMovie) -> TileFacts<'_> {
    TileFacts {
        owner: m as *const PmsMovie as usize,
        src: m.sid.raw(),
        kind: kind_of(m.kind),
        title: &m.title,
        show_title: &m.show_title,
        thumb: &m.thumb,
        still: &m.still,
        art: &m.art,
        year: m.year,
        season_index: m.season_index,
        ep_index: m.ep_index,
        child_count: m.child_count,
        resume: resume_of(m),
        watched: m.watched,
        unwatched: m.unwatched,
        // Only a collection's thumb can be a composite, and only a collection tile reads the
        // flag, so the path is parsed for those alone rather than for every tile every frame.
        composite_thumb: m.kind == KIND_COLLECTION && is_composite_thumb(&m.thumb),
        blur: m.has_blur.then_some(m.blur),
        subtype: m.subtype.as_deref(),
        smart: m.smart,
        genre: m.genre.as_deref(),
        video_resolution: m.video_resolution.as_deref(),
        hdr: m.hdr,
        audience_rating: m.audience_rating,
        leaf_count: m.leaf_count,
        viewed_leaf_count: m.viewed_leaf_count,
        added_at: m.added_at,
        // No surface opts in here: a deck builds its own keys and sets this itself.
        deck: None,
    }
}

/// **The two store keys a Continue Watching deck card's artwork is asked by**, built ONCE per deck
/// row (by the screen that owns the deck, when its publication lands) and held beside the row, so a
/// frame builds no string: the deck's cards read them through [`for_deck`] and a library grid, which
/// never opts in, pays for none.
///
/// * `cover`: [`artwork::cover_path`] of the title's CATALOG id, by the Watchlist's own rule
///   ([`PmsMovie::watchlist_subject`]): a movie or a show is itself, an episode (or a season) is its
///   SHOW, so every episode of a show asks for the show's cover. EMPTY when the row has no catalog
///   id (a local agent, personal media, a guid that is not `plex://movie|show` with 24 hex digits,
///   an episode whose listing lacks `grandparentGuid`): such a card has no cover to ask for and goes
///   straight to the composite.
/// * `card`: [`artwork::card_path`], the composite of the row's background `art` and its title's
///   clearLogo path. Episodes and seasons carry the show's logo. EMPTY when the row has no
///   background or logo source.
///
/// A cover is server-independent (one catalog id: the same title held by two servers asks for one
/// cover) and a composite is server-local (its key names a path on `TileFacts::src`'s server), so a
/// merged deck's two copies of one title share a cover and keep two composites.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeckKeys {
    pub cover: String,
    pub card: String,
}

impl DeckKeys {
    /// The keys of one row. Pure: a function of the row's title identity and artwork paths.
    pub fn of(item: &PmsMovie) -> Self {
        let cover = item
            .watchlist_subject()
            .and_then(|(_, hex, _)| hex)
            .and_then(|hex| artwork::cover_path(&plx_data::watchlist::encode_hex(&hex)))
            .unwrap_or_default();
        // The logo is the TITLE's: an episode or a season carries no clearLogo of its own.
        let logo_rk = if matches!(item.kind, 2 | 3) { item.show_rk.as_str() } else { item.rk.as_str() };
        let card = artwork::card_path(item.logo.as_deref().unwrap_or(logo_rk), &item.art)
            .unwrap_or_default();
        Self { cover, card }
    }

    /// The keys of a deck's cards, in the deck's order.
    pub fn of_all<'a>(items: impl IntoIterator<Item = &'a PmsMovie>) -> Vec<Self> {
        items.into_iter().map(Self::of).collect()
    }
}

/// What `ui` reads of a Continue Watching deck card: [`of`]'s facts, opted in to the deck card's
/// artwork walk (Plex's cover, our composite, then the title in type: `plx_ui::card_art`) with the
/// keys the deck built for the row ([`DeckKeys`]). The one place a surface opts in.
pub fn for_deck<'a>(m: &'a PmsMovie, keys: &'a DeckKeys) -> TileFacts<'a> {
    TileFacts { deck: Some(DeckArt { cover: &keys.cover, card: &keys.card }), ..of(m) }
}

/// **A Continue Watching still's footer and scrim on `tile`: the one drawing every surface of the
/// deck uses** (Home's row, its fading ghosts, the Library's Recommended Continue Watching row).
/// The cue is what OK does, from the one deck-press rule: a play glyph while the Playback setting
/// says OK plays, an info glyph while it opens the page; it opens with the card's own pop.
pub fn deck_overlay(
    p: plx_ui::Painter,
    facts: &TileFacts<'_>,
    tile: &plx_ui::cards::Tile,
    measure: &dyn plx_machine::machine::Measure,
) {
    let cue = if plx_media::route::deck_press().press_plays() { plx_ui::still_footer::Cue::Play } else { plx_ui::still_footer::Cue::Info };
    plx_ui::widgets::still_overlay(p, facts, tile.rect, tile.radius, plx_ui::widgets::StillFooter::Continue { cue, pop: tile.pop }, measure);
}

/// Whether `thumb` is the server's generated composite of a collection
/// (`/library/collections/{rk}/composite/{stamp}`), the thumb the poster store bakes into our fan
/// and whose collection name the card then sets live. A custom poster, or no art at all, is not.
pub fn is_composite_thumb(thumb: &str) -> bool {
    plx_plex::plex::collections::composite_parts(thumb).is_some()
}

/// The row's numeric kind (`0` movie, `1` show, `2` season, `3` episode, `4` collection), named.
/// A value outside that table is [`TileKind::Other`], which the library reads as none of the three
/// it draws differently.
fn kind_of(kind: c_int) -> TileKind {
    match kind {
        0 => TileKind::Movie,
        1 => TileKind::Show,
        2 => TileKind::Season,
        3 => TileKind::Episode,
        KIND_COLLECTION => TileKind::Collection,
        _ => TileKind::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use plx_ui::widgets::{poster_mark, row_watch_state, PosterMark};

    /// A MOVIE row at a given watched state. `dur_ns` is 100 min, so `resume_ms` reads as a
    /// percentage of the way in. For a LEAF the two flags really are each other's negation.
    fn row(watched: bool, resume_ms: i64) -> PmsMovie {
        let mut m = PmsMovie::default();
        m.dur_ns = 100 * 60 * 1000 * 1_000_000;
        m.watched = watched;
        m.unwatched = !watched;
        m.resume_ms = resume_ms;
        m
    }

    #[test]
    fn the_kind_table_is_named_and_an_unknown_kind_is_other() {
        let kind = |k: c_int| of(&PmsMovie { kind: k, ..Default::default() }).kind;
        assert_eq!(kind(0), TileKind::Movie);
        assert_eq!(kind(1), TileKind::Show);
        assert_eq!(kind(2), TileKind::Season);
        assert_eq!(kind(3), TileKind::Episode);
        assert_eq!(kind(KIND_COLLECTION), TileKind::Collection);
        assert_eq!(kind(9), TileKind::Other);
    }

    /// A season is known by its SHOW, and its own title is the identity the details fall back to;
    /// every other kind is known by its own title.
    #[test]
    fn a_season_is_captioned_by_its_show_and_keeps_its_own_title_as_its_identity() {
        let season = PmsMovie { kind: 2, title: "Season 6".into(), show_title: "The Office".into(), season_index: 6, ..Default::default() };
        let f = caption_facts(&season);
        assert_eq!(f.title.as_deref(), Some("The Office"));
        assert_eq!(f.season, Some((6, "Season 6".to_owned())));
        let orphan = PmsMovie { show_title: String::new(), ..season.clone() };
        assert_eq!(caption_facts(&orphan).title.as_deref(), Some("Season 6"), "no show to name: its own title");
        let episode = PmsMovie { kind: 3, title: "Pilot".into(), show_title: "Severance".into(), ..Default::default() };
        let f = caption_facts(&episode);
        assert_eq!((f.title.as_deref(), f.season), (Some("Pilot"), None), "an episode is known by its own title");
    }

    #[test]
    fn the_facts_are_the_rows_own_values_and_remember_which_row() {
        let m = PmsMovie {
            title: "Pilot".into(),
            show_title: "Show".into(),
            thumb: "/poster".into(),
            still: "/still".into(),
            art: "/art".into(),
            year: 1999,
            season_index: 2,
            ep_index: 5,
            child_count: 7,
            kind: 3,
            ..Default::default()
        };
        let f = of(&m);
        assert_eq!(f.owner, &m as *const PmsMovie as usize);
        assert_eq!(f.src, m.sid.raw());
        assert_eq!((f.title, f.show_title), ("Pilot", "Show"));
        assert_eq!((f.thumb, f.still, f.art), ("/poster", "/still", "/art"));
        assert_eq!((f.year, f.season_index, f.ep_index, f.child_count), (1999, 2, 5, 7));
        assert_eq!(f.kind, TileKind::Episode);
    }

    /// The facts carry the row's own resume answer: a time left that is the runtime less the
    /// offset, present exactly when `resume_frac` says the item is in progress.
    #[test]
    fn progress_is_the_rows_resume_rule_with_the_time_left() {
        let m = row(false, 30 * 60 * 1000);
        let resume = of(&m).resume.expect("30 of 100 minutes is in progress");
        assert_eq!(Some(resume.frac), m.resume_frac());
        assert_eq!(resume.left_ms, 70 * 60 * 1000);
        assert_eq!(of(&row(false, 0)).resume, None);
    }

    /// A row with no blur colours lends the page nothing; one with them hands over all four corners.
    #[test]
    fn a_rows_blur_is_present_only_when_it_has_one() {
        assert_eq!(of(&PmsMovie::default()).blur, None);
        let blur = [[0.1, 0.2, 0.3]; 4];
        let m = PmsMovie { has_blur: true, blur, ..Default::default() };
        assert_eq!(of(&m).blur, Some(blur));
    }

    /// Only a server composite is a fan; a collection with a custom thumb, or none, is not. (The
    /// path rule itself is `plex::collections::composite_parts`'s, graded there.)
    #[test]
    fn only_a_collections_server_composite_is_a_fan() {
        let collection = |thumb: &str| {
            let m = PmsMovie { kind: KIND_COLLECTION, thumb: thumb.into(), ..Default::default() };
            of(&m).composite_thumb
        };
        assert!(collection("/library/collections/7/composite/1700000000?width=400"));
        assert!(!collection("/library/metadata/7/thumb/1700000000"));
        assert!(!collection(""));
        assert!(is_composite_thumb("/library/collections/7/composite/1700000000?width=400"));
        assert!(!is_composite_thumb("/library/metadata/7/thumb/1700000000"));
        assert!(!is_composite_thumb(""));
        // a composite path on anything but a collection is not read as one: no tile asks
        let film = PmsMovie { thumb: "/library/collections/7/composite/1".into(), ..Default::default() };
        assert!(!of(&film).composite_thumb);
    }

    // ── The poster mark over a REAL row: the resume rule and the mark must agree ────────────────
    //
    // `ui::widgets`' own tests grade the mark's CHOICE over plain facts. These grade the half that
    // needs the row: that what the row's `resume_frac` calls in progress is what the facts say, so
    // the bar the caller draws and the mark the card wears can never disagree.

    #[test]
    fn a_re_watch_in_flight_draws_a_bar_and_wears_no_disc() {
        // PMS reports BOTH on a finished-then-restarted item; the bar wins.
        let m = row(true, 30 * 60 * 1000);
        let f = of(&m);
        assert_eq!(poster_mark(&f), PosterMark::InProgress);
        assert!(m.resume_frac().is_some(), "InProgress must be exactly when the caller draws the bar");
    }

    #[test]
    fn an_offset_the_server_never_cleared_is_finished_not_in_progress() {
        // resume AT or PAST the end: a full-width bar there read as a rendering bug, and it would
        // also hide the disc the item has earned. Both the mark and the bar must agree.
        for resume in [100 * 60 * 1000, 200 * 60 * 1000] {
            let m = row(true, resume);
            assert_eq!(m.resume_frac(), None, "resume {resume} must not draw a bar");
            assert_eq!(poster_mark(&of(&m)), PosterMark::Watched, "resume {resume}");
        }
    }

    #[test]
    fn a_row_with_no_runtime_cannot_be_in_progress() {
        // dur_ns == 0 (the server sent no duration): a fraction is undefined, so there is no bar to
        // draw and the watched flag alone decides.
        let mut m = row(true, 30 * 60 * 1000);
        m.dur_ns = 0;
        assert_eq!(m.resume_frac(), None);
        assert_eq!(poster_mark(&of(&m)), PosterMark::Watched);
        let mut m = row(false, 30 * 60 * 1000);
        m.dur_ns = 0;
        assert_eq!(poster_mark(&of(&m)), PosterMark::None);
    }

    // ---- the Continue Watching deck's keys (`DeckKeys`, `for_deck`) -------------------------------

    use plx_plex::plex::ServerId;
    use plx_ui::card_art::{needs_type_title, pick, ArtProbe, CardRung};

    const MOVIE_HEX: &str = "5d776824999c4a001f9a1b2c";
    const SHOW_HEX: &str = "5d9c086c46115600200aa2fe";

    fn bytes(hex: &str) -> [u8; 12] {
        plx_data::watchlist::decode_hex(hex).expect("24 hex digits")
    }

    fn movie() -> PmsMovie {
        PmsMovie {
            kind: 0, sid: ServerId::from_raw(1), rk: "605".into(), title: "Heat".into(),
            art: "/library/metadata/605/art/1778546279".into(), thumb: "/library/metadata/605/thumb/1".into(),
            guid: Some(bytes(MOVIE_HEX)), ..Default::default()
        }
    }

    /// An episode: its OWN guid and rating key are not its show's, and its `art` is the show's.
    fn episode() -> PmsMovie {
        PmsMovie {
            kind: 3, sid: ServerId::from_raw(1), rk: "9001".into(), title: "Pilot".into(), show_title: "Severance".into(),
            show_rk: "77".into(), guid: Some([9; 12]), show_guid: Some(bytes(SHOW_HEX)),
            art: "/library/metadata/77/art/5".into(), still: "/library/metadata/9001/thumb/2".into(),
            thumb: "/library/metadata/77/thumb/3".into(), ..Default::default()
        }
    }

    /// A probe answering from a table of settled misses.
    struct Missed(&'static [&'static str]);
    impl ArtProbe for Missed {
        fn failed(&self, _srv: u16, path: &str) -> bool { self.0.contains(&path) }
    }

    #[test]
    fn a_movies_keys_are_its_own_catalog_id_and_its_own_background_and_logo() {
        let keys = DeckKeys::of(&movie());
        assert_eq!(keys.cover, format!("/plx/cover/{MOVIE_HEX}"));
        assert_eq!(keys.card, "/plx/card/605/library/metadata/605/art/1778546279");
    }

    /// An episode asks for its SHOW: the show's catalog id (`grandparentGuid`, never the episode's
    /// own guid) and the show's clearLogo (`show_rk`, never the episode's rating key), over the
    /// background the episode carries, which is the show's.
    #[test]
    fn an_episodes_keys_are_its_shows() {
        let keys = DeckKeys::of(&episode());
        assert_eq!(keys.cover, format!("/plx/cover/{SHOW_HEX}"));
        assert_eq!(keys.card, "/plx/card/77/library/metadata/77/art/5");
        let a_show = PmsMovie { kind: 1, rk: "77".into(), show_rk: String::new(), guid: Some(bytes(SHOW_HEX)),
            art: "/library/metadata/77/art/5".into(), ..Default::default() };
        assert_eq!(DeckKeys::of(&a_show), keys, "a show row and its episode ask for the same two pictures");
    }

    #[test]
    fn a_changed_show_logo_rekeys_the_episode_composite_without_an_art_change() {
        let from_pms = |images| {
            let metadata = serde_json::from_value(serde_json::json!({
                "type": "episode", "ratingKey": "9001", "grandparentRatingKey": "77",
                "art": "/library/metadata/77/art/5", "Image": images,
            })).unwrap();
            plx_data::pms::parse_item(&metadata, ServerId::from_raw(1))
        };
        let before = "/library/metadata/77/clearLogo/17";
        let after = "/library/metadata/77/clearLogo/18";
        let keys = |path| DeckKeys::of(&from_pms(serde_json::json!([
            {"type": "background", "url": "/library/metadata/77/art/5"},
            {"type": "clearLogo", "url": "/library/metadata/9001/clearLogo/12"},
            {"type": "clearLogo", "url": path},
        ])));
        let (old, new) = (keys(before), keys(after));
        assert_ne!(old.card, new.card);
        assert_eq!(artwork::card_parts(&old.card), Some((before, "/library/metadata/77/art/5")));
        assert_eq!(artwork::card_parts(&new.card), Some((after, "/library/metadata/77/art/5")));
        let removed = from_pms(serde_json::json!([{"type": "background"}]));
        assert!(DeckKeys::of(&removed).card.is_empty(), "a removed logo has no composite");
    }

    /// A guid-less item (a local agent, personal media: `guid` is `None` when it is not a 24-hex
    /// `plex://movie|show`) has no cover to ask for and goes straight to the composite; an item with
    /// no background or nothing to ask the logo of has no composite either.
    #[test]
    fn an_item_with_no_catalog_id_has_no_cover_and_still_has_a_composite() {
        let local = PmsMovie { guid: None, ..movie() };
        let keys = DeckKeys::of(&local);
        assert_eq!(keys.cover, "");
        assert_eq!(keys.card, "/plx/card/605/library/metadata/605/art/1778546279");
        let orphan = PmsMovie { show_guid: None, ..episode() };
        assert_eq!(DeckKeys::of(&orphan).cover, "", "an episode whose listing lacks grandparentGuid");
        assert_eq!(DeckKeys::of(&orphan).card, "/plx/card/77/library/metadata/77/art/5");
        assert_eq!(DeckKeys::of(&PmsMovie { art: String::new(), ..movie() }).card, "", "no background");
        assert_eq!(DeckKeys::of(&PmsMovie { show_rk: String::new(), ..episode() }).card, "", "no show to ask the logo of");
        assert_eq!(DeckKeys::of(&PmsMovie { art: "/a?X-Plex-Token=SECRET".into(), ..movie() }).card, "",
            "a key never carries a query");
    }

    /// The same title held by two servers (a merged deck): one catalog id, so one cover; each
    /// server's composite names its own paths, and each card draws through its own server.
    #[test]
    fn the_same_title_on_two_servers_shares_a_cover_and_keeps_two_composites() {
        let a = movie();
        let b = PmsMovie { sid: ServerId::from_raw(2), rk: "88".into(), art: "/library/metadata/88/art/2".into(), ..movie() };
        let (ka, kb) = (DeckKeys::of(&a), DeckKeys::of(&b));
        assert_eq!(ka.cover, kb.cover);
        assert_ne!(ka.card, kb.card);
        assert_eq!(kb.card, "/plx/card/88/library/metadata/88/art/2");
        let (fa, fb) = (for_deck(&a, &ka), for_deck(&b, &kb));
        assert_ne!(fa.src, fb.src, "each copy asks its own server's transcoder");
        assert_eq!(fa.deck.unwrap().cover, fb.deck.unwrap().cover);
    }

    /// The keys are what the store keys parse back to, and no key carries a credential or a query.
    #[test]
    fn the_keys_are_the_ones_the_poster_source_parses() {
        for item in [movie(), episode()] {
            let keys = DeckKeys::of(&item);
            assert!(artwork::cover_parts(&keys.cover).is_some() && artwork::card_parts(&keys.card).is_some());
            assert!(!keys.cover.contains('?') && !keys.card.contains('?'));
        }
        assert_eq!(DeckKeys::of_all([&movie(), &episode()]).len(), 2);
    }

    /// A surface that did not opt in keeps its old answers: `of` carries no deck, and `for_deck` is
    /// `of` plus exactly the two keys.
    #[test]
    fn only_for_deck_opts_in_and_changes_nothing_else() {
        let m = episode();
        let keys = DeckKeys::of(&m);
        assert_eq!(of(&m).deck, None);
        let deck = for_deck(&m, &keys);
        assert_eq!(deck.deck, Some(DeckArt { cover: &keys.cover, card: &keys.card }));
        assert_eq!(format!("{:?}", TileFacts { deck: None, ..deck }), format!("{:?}", of(&m)));
        // An opted-in card whose keys are both empty is still opted in.
        let bare = DeckKeys::default();
        assert_eq!(for_deck(&m, &bare).deck, Some(DeckArt::default()));
    }

    /// **An episode in Continue Watching draws its SHOW's art**: the cover, then the show composite,
    /// then (both settled as misses) type on the show's background, then the episode's still, then
    /// the poster. The detail page's strip, which does not opt in, keeps the still.
    #[test]
    fn a_continue_watching_episode_walks_the_show_art_order() {
        let m = episode();
        let keys = DeckKeys::of(&m);
        let f = for_deck(&m, &keys);
        let got = |missed: &'static [&'static str]| pick(&f, &Missed(missed)).map(|a| (a.path, a.rung)).unwrap();
        let (cover, card) = (keys.cover.as_str(), keys.card.as_str());
        assert_eq!(got(&[]), (cover, CardRung::Cover));
        // `Missed` wants 'static strings; the keys here are fixed
        let cover_miss: &'static [&'static str] = &["/plx/cover/5d9c086c46115600200aa2fe"];
        assert_eq!(cover, cover_miss[0]);
        assert_eq!(got(cover_miss), (card, CardRung::Composite));
        let both: &'static [&'static str] = &["/plx/cover/5d9c086c46115600200aa2fe", "/plx/card/77/library/metadata/77/art/5"];
        assert_eq!(got(both), ("/library/metadata/77/art/5", CardRung::Type), "the show's background, not the still");
        assert!(needs_type_title(&f, &Missed(both)));
        assert!(!needs_type_title(&f, &Missed(&[])), "a cover names itself");
        // no show background: the still, then the show poster
        let no_art = PmsMovie { art: String::new(), ..m.clone() };
        let k = DeckKeys::of(&no_art);
        assert_eq!(k.card, "");
        let f = for_deck(&no_art, &k);
        assert_eq!(pick(&f, &Missed(cover_miss)).map(|a| (a.path, a.rung)), Some(("/library/metadata/9001/thumb/2", CardRung::Type)));
        let no_still = PmsMovie { still: String::new(), ..no_art };
        let f = for_deck(&no_still, &k);
        assert_eq!(pick(&f, &Missed(cover_miss)).map(|a| a.path), Some("/library/metadata/77/thumb/3"));
        // the strip's facts did not opt in
        assert_eq!(of(&m).deck, None);
        assert!(!needs_type_title(&of(&m), &Missed(both)));
    }

    /// A LEAF is delegated whole, so every resume-point edge `poster_mark` keeps — an offset past
    /// the end is finished, a row with no runtime cannot be in progress — holds for the menu too
    /// without being restated.
    #[test]
    fn a_leaf_asks_the_poster_and_gets_its_answer_unchanged() {
        for m in [
            row(false, 0),
            row(true, 0),
            row(false, 30 * 60 * 1000),
            row(true, 30 * 60 * 1000),
            row(true, 200 * 60 * 1000),
        ] {
            let f = of(&m);
            assert_eq!(row_watch_state(&f), poster_mark(&f), "a leaf must not answer twice");
        }
    }

    /// **The deck's footer cue says what OK does**, read from the one deck-press rule
    /// (`deck_press().press_plays()`): the Play glyph while OK plays, the Info glyph while it opens
    /// the page (the default). Drawn at full pop, the cue's slot is open and the glyph is painted;
    /// swapping the two arms fails.
    #[test]
    fn the_deck_footer_cue_is_the_play_glyph_under_play_and_the_info_glyph_under_details() {
        use plx_media::route::{deck_press, restore_deck_press, DeckPress};
        use plx_ui::icons::Icon;
        struct Restore(DeckPress);
        impl Drop for Restore {
            fn drop(&mut self) {
                restore_deck_press(self.0);
            }
        }
        let _g = plx_base::testlock::serial();
        let _restore = Restore(deck_press());
        let m = row(false, 30 * 60 * 1000);
        let facts = of(&m);
        let tile = plx_ui::cards::Tile { rect: plx_ui::Rect::new(0.0, 0.0, 420.0, 236.0), scale: 1.0, radius: 12.0, focused: true, pop: 1.0 };
        let measure = plx_ui::fixture::FixtureMeasure;
        for (mode, shown, absent) in [(DeckPress::Play, Icon::Play, Icon::Info), (DeckPress::Details, Icon::Info, Icon::Play)] {
            restore_deck_press(mode);
            let icons: Vec<Icon> = plx_ui::draw_census::capture_icons(|| deck_overlay(plx_ui::Painter::recording(), &facts, &tile, &measure))
                .into_iter()
                .map(|(icon, _)| icon)
                .collect();
            assert!(icons.contains(&shown), "{mode:?}: the footer cue is {shown:?}, drew {icons:?}");
            assert!(!icons.contains(&absent), "{mode:?}: no {absent:?} cue beside it, drew {icons:?}");
        }
    }
}
