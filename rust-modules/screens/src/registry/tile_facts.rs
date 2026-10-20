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
use plx_ui::tile::{Resume, TileFacts, TileKind};

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
        resume: m.resume_frac().map(|frac| Resume {
            frac,
            left_ms: m.dur_ns / 1_000_000 - m.resume_ms,
        }),
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
    }
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
}
