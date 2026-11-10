//! **Under posters** — what a focused poster's caption band shows and how tall that band is.
//!
//! Settings > Libraries > Under posters: a Title switch (default Off) and, per kind of title, a SET
//! of detail parts ([`DetailParts`]: year, length, seasons, genre, video quality, age rating, score;
//! at most four, shown in a fixed order and joined with " · "). Only the FOCUSED row opens its band: 80 px for one line, 112 px for two,
//! nothing when the policy shows nothing; non-focused rows keep their 40 px spacing. (Recommended's Continue Watching
//! poster deck is one shelf handed a different policy: its progress line is always shown, so under Nothing
//! it is handed `registry::captions::deck_policy` and keeps its one line; the Watchlist is the other:
//! its tiles' own status words show with no part ticked, so its band is reserved by
//! `registry::captions::status_policy` and opens 80 px under an empty set.)
//!
//! **A line the setting turns on is never drawn empty**, whatever the tile is. The title line is the
//! name a reader knows the tile by (a season by its SHOW, an episode by its own title). The details
//! line is the ticked parts' facts, and a tile that has none of them (a season has no length and
//! often no genre or rating) says its identity line instead (a season "Season 6" or "Specials", an
//! episode its "S1 · E8" address, a show or movie its year, a collection its size), and a tile with
//! not even that says what KIND it is ("Episode", "Movie", "Show", "Collection"), so a band the
//! policy opens is never open over nothing. With no part ticked and Title Off no band opens at all (but for the deck and the Watchlist's status line).
//!
//! **One collection rule, on every surface.** A collection's details line is its size ("7 films")
//! under every set of parts but the empty one; the Title switch governs its title line like any poster's;
//! and a name its face cuts ([`CaptionFacts::name_cut`]) is told on the details line when the title
//! line is off, since the face is the only other place it is told.
//!
//! **Poster tiles follow this policy except a movie or TV show library grid showing its sort value**
//! (one value line under every card, `card_row::draw_sort_value`): the Library grid, the Collections page, the
//! Watchlist, Home's shelves, the Library's Recommended shelves, Search's poster rows, the person
//! page's filmography, Detail's Related and collection shelves and the Collection screen's members
//! (the screens build the focused block through `registry::captions`, and hand a
//! [`crate::cards::Shelf`] / grid the policy so the room it reserves is [`band_h`] of the same value).
//! **Landscape tiles and headshots are not posters and are not on it**: an episode still (the
//! Library's Episodes listing, Search's Episode row, Recommended's landscape rows, Detail's Extras)
//! and a cast or search-result headshot keep their own block, in their own height (the legacy band,
//! `Grid::set_caption_policy(None)` for a grid), under every setting. The one exception is the
//! Library's Episodes grid: while Sort value is On and the sort has a value for episodes it draws its
//! sort value line under every still INSTEAD of that block (rows 60 px apart, the focused one opening
//! 26 px), whatever its policy; the `None` policy sizes only the Off / no-value case (the legacy
//! 114 px band). The types and the two function signatures are what the
//! library, collection and settings packages compile against. [`band_h`] sizes the band;
//! [`caption_lines`] words the lines in the given locale, with the catalog's plurals.

pub use plx_base::detail_parts::{DetailPart, DetailParts};
use plx_platform::i18n::LocaleContext;

/// Band height with nothing to show.
pub const BAND_NONE: f32 = 0.0;
/// Band height for one caption line.
pub const BAND_ONE_LINE: f32 = 80.0;
/// Band height for two caption lines.
pub const BAND_TWO_LINES: f32 = 112.0;

/// The setting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CaptionPolicy {
    /// Show the title above the details.
    pub title: bool,
    /// The parts of the details line under a movie (and under every tile that is not a TV show: a
    /// season, an episode, a collection).
    pub details: DetailParts,
    /// The parts of the details line under a TV show ([`CaptionFacts::is_show`]): a show has seasons
    /// and no length or video quality.
    pub show_details: DetailParts,
}

impl Default for CaptionPolicy {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl CaptionPolicy {
    pub const DEFAULT: CaptionPolicy =
        CaptionPolicy { title: false, details: DetailParts::MOVIE_DEFAULT, show_details: DetailParts::SHOW_DEFAULT };

    /// The same parts under every kind of tile.
    pub const fn same(title: bool, details: DetailParts) -> CaptionPolicy {
        CaptionPolicy { title, details, show_details: details }
    }

    /// The parts a tile is worded by: the show set for a TV show, the movie set for the rest.
    pub fn details_for(&self, is_show: bool) -> DetailParts {
        if is_show { self.show_details } else { self.details }
    }

    /// The policy of a surface that lists ONE kind of tile (a Movies or Shows grid, Seasons,
    /// Collections): both sets are that kind's own, so the band opens for what the kind shows and
    /// not for the other kind's line ([`Self::max_lines`] holds the room for either kind, which only a
    /// mixed shelf needs).
    pub fn only(&self, is_show: bool) -> CaptionPolicy {
        CaptionPolicy::same(self.title, self.details_for(is_show))
    }

    /// How many lines this policy can show at most (a tile with no name, or no fact and no identity,
    /// may leave fewer). The band is the shelf's, never a tile's, so it opens for either kind's
    /// line: a shelf mixing movies and shows holds the room for whichever has one. A surface that
    /// lists one kind narrows first ([`Self::only`]).
    pub fn max_lines(&self) -> usize {
        usize::from(self.title) + usize::from(!self.details.is_empty() || !self.show_details.is_empty())
    }
}

/// The focused caption band's height: 0 / 80 / 112.
pub fn band_h(policy: &CaptionPolicy) -> f32 {
    match policy.max_lines() {
        0 => BAND_NONE,
        1 => BAND_ONE_LINE,
        _ => BAND_TWO_LINES,
    }
}

/// Everything a caption can be built from, as plain values; the caller copies them from its row.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CaptionFacts {
    pub title: Option<String>,
    pub year: Option<i32>,
    pub duration_min: Option<u32>,
    /// A partly-watched item's position, for "Nm left".
    pub view_offset_min: Option<u32>,
    pub genre: Option<String>,
    pub video_resolution: Option<String>,
    pub hdr: bool,
    pub content_rating: Option<String>,
    /// 0..10.
    pub audience_rating: Option<f32>,
    pub leaf_count: Option<u32>,
    pub viewed_leaf_count: Option<u32>,
    pub season_count: Option<u32>,
    pub child_count: Option<u32>,
    /// A collection's subtype (`movie`, `show`, …).
    pub subtype: Option<String>,
    pub is_show: bool,
    /// A movie: with no year to be known by, its details line says what it is.
    pub is_movie: bool,
    /// Where a tile came from when that is not the household's own server (a friend's server handle,
    /// on a Search result): credited after the details line, so two servers' copies of one title
    /// read apart. Dropped with the details line under Nothing.
    pub source: Option<String>,
    /// An episode's `(season, episode)` numbers (`0` for one the server did not number), which its
    /// default details line says in place of a year and length.
    pub episode: Option<(u32, u32)>,
    /// A SEASON tile's `(number, its own title)`: `0` for a season the server did not number, and the
    /// title the server gave it ("Season 6", "Specials"). `title` then holds the name of its SHOW, the
    /// name a reader knows the tile by, and this is the season's identity line.
    pub season: Option<(u32, String)>,
    /// An episode's or a season's SHOW name, which the details line says when the tile's art already
    /// prints its address ([`address_on_art`](Self::address_on_art)).
    pub show: Option<String>,
    /// An episode or a season whose poster already wears its address as a persistent mark ("S3 · E4",
    /// "SEASON 3"; the Collection screen's members, `widgets::poster_label`): its details line does
    /// not repeat it, it names the show, and a season with no show says nothing rather than its own
    /// number twice.
    pub address_on_art: bool,
    /// A COLLECTION whose face cuts its name (`collection_tile::name_is_cut`: the live name over a
    /// baked fan or the neutral tile runs past its band). The face is the only other place the name
    /// is told, so with the Title switch off the details line carries it ("Name · 7 films"): the
    /// band is still the policy's, one line, and the cut name is never lost.
    pub name_cut: bool,
}

/// The caption's lines for `facts` under `policy`: `[title, details]` with absent lines `None`.
///
/// **One rule for every kind of tile: a line the setting turns on is never drawn empty.** The title
/// line is `facts.title`, the name a reader knows the tile by (the caller puts a season's SHOW there).
/// The details line is the ticked parts' facts, and a tile that has none of them (a season has no
/// length and often no genre or rating, a movie may have no genre) says its identity line instead
/// ([`identity_line`]; a movie, show or collection with not even that says what KIND it is, the last
/// fallback), so a band the policy opens is never open over nothing, and the source credit
/// ([`CaptionFacts::source`]) follows whatever the details line says. With no part ticked there is
/// no details line, no fallback and no credit; a line is `None` only when the setting turns it off
/// (or the tile has no name or identity at all), never for want of one ticked fact. The ticked parts
/// that have a value are joined in their fixed order ([`DetailParts::iter`]).
///
/// Every word on a line comes from the catalog in `locale` (the shared `core.*` / `browse.*`
/// duration, count and plural keys plus `captions.new_episodes`); the only literals are the
/// technical marks `HDR`, `4K`, `1080p` and the middle dot.
pub fn caption_lines(policy: &CaptionPolicy, facts: &CaptionFacts, locale: &LocaleContext) -> [Option<String>; 2] {
    let title = policy.title.then(|| facts.title.clone()).flatten().filter(|t| !t.is_empty());
    let parts = policy.details_for(facts.is_show);
    if parts.is_empty() {
        return [title, None];
    }
    // A COLLECTION has one fact worth a line, its size, under every set of parts (it has no genre,
    // stream or age rating to pick): one rule on every surface that shows one. A name its face cuts
    // is the only other place the name is told, so with the title line off it rides the details line.
    if facts.subtype.is_some() {
        let size = identity_line(facts, locale);
        let name = facts.title.clone().filter(|name| facts.name_cut && title.is_none() && !name.is_empty());
        return [title, join(name.into_iter().chain(size).chain(facts.source.clone().filter(|s| !s.is_empty())).collect())];
    }
    // What matters more than the year, length and seasons (a season's own title, an episode's
    // address, a show's new episodes, a started movie's time left) stands in for the line when the
    // viewer ticked any of those three parts; a line of genre, video, age rating or score alone has
    // no such stand-in.
    let status = [DetailPart::Year, DetailPart::Length, DetailPart::Seasons]
        .iter()
        .any(|p| parts.contains(*p))
        .then(|| status_line(facts, locale))
        .flatten();
    let picked = status.or_else(|| join(parts.iter().filter_map(|part| part_value(part, facts, locale)).collect()));
    let details = picked.or_else(|| identity_line(facts, locale));
    // The same words are never drawn twice: a season named by its show over its show, or an orphan
    // season over its own title, shows the one line.
    let details = details.filter(|line| title.as_ref() != Some(line));
    [title, join(details.into_iter().chain(facts.source.clone().filter(|s| !s.is_empty())).collect())]
}

/// **One part's value for `facts`**, as the line says it: the year, the length ("1h 52m"), the season
/// count ("6 seasons"), the first genre, the video quality ("4K · HDR"), the age rating, the audience
/// score ("8.1"). `None` when the tile has no such fact. The Under posters picker words each row's
/// sample through this, so a row and the line it ticks can never word a fact differently.
pub fn part_value(part: DetailPart, facts: &CaptionFacts, locale: &LocaleContext) -> Option<String> {
    use plx_platform::i18n::msg;
    match part {
        DetailPart::Year => facts.year.filter(|y| *y > 0).map(|y| y.to_string()),
        DetailPart::Length => facts.duration_min.filter(|m| *m > 0).map(|d| crate::fmt::duration_minutes_in(locale, i64::from(d))),
        DetailPart::Seasons => facts.season_count.or(facts.child_count).filter(|n| *n > 0)
            .map(|n| msg::browse_detail_seasons_in(locale, i64::from(n))),
        DetailPart::Genre => facts.genre.clone().filter(|g| !g.is_empty()),
        DetailPart::VideoQuality => join(
            facts.video_resolution.as_deref().and_then(resolution_mark).into_iter()
                .chain(facts.hdr.then(|| "HDR".to_string())).collect(),
        ),
        DetailPart::AgeRating => facts.content_rating.clone().filter(|r| !r.is_empty()),
        DetailPart::Score => facts.audience_rating.filter(|s| *s > 0.0).map(|s| locale.decimal((s * 10.0).round() as i64, 1)),
    }
}

/// The facts of one caption line joined with the app's one separator (" · "); no facts, no line.
pub fn join(parts: Vec<String>) -> Option<String> {
    (!parts.is_empty()).then(|| parts.join(" \u{b7} "))
}

/// PMS `videoResolution` ("4k", "1080", "720", "sd") as the mark a viewer knows.
fn resolution_mark(raw: &str) -> Option<String> {
    let r = raw.trim().to_ascii_lowercase();
    match r.as_str() {
        "" => None,
        "4k" => Some("4K".to_string()),
        "sd" => Some("SD".to_string()),
        n if n.bytes().all(|b| b.is_ascii_digit()) => Some(format!("{n}p")),
        other => Some(other.to_string()),
    }
}

/// An episode's address, "S1 · E8" (or "E8" for one the server did not give a season): the app's one
/// spelling of it ([`crate::fmt::episode_address_in`]), the same as the art's mark and the still
/// overlay. `None` for an episode the server did not number.
fn episode_address(f: &CaptionFacts, cx: &LocaleContext) -> Option<String> {
    let (season, ep) = f.episode.filter(|(_, ep)| *ep > 0)?;
    Some(crate::fmt::episode_address_in(cx, i64::from(season), i64::from(ep)))
}

/// What an EPISODE's caption says first: its address, "S1 · E8", unless its art already prints that
/// ([`CaptionFacts::address_on_art`]), in which case the show it belongs to (the address, when the
/// server named no show: the repeat beats an empty line). `None` for an episode with neither.
fn episode_line(f: &CaptionFacts, cx: &LocaleContext) -> Option<String> {
    f.episode?;
    f.address_on_art.then(|| f.show.clone().filter(|show| !show.is_empty())).flatten()
        .or_else(|| episode_address(f, cx))
}

/// **The line that stands in for the year, length and seasons** when a tile has something that
/// matters more: a season is its own identity ("Season 6": it beats its year, which names neither
/// the show nor the season), an episode its address, a show in progress its unwatched new episodes,
/// a started movie the time it has left. `None` for a tile with none of those, which the parts word.
fn status_line(f: &CaptionFacts, cx: &LocaleContext) -> Option<String> {
    use plx_platform::i18n::msg;
    if f.season.is_some() {
        return identity_line(f, cx);
    }
    if let Some(line) = episode_line(f, cx) {
        return Some(line);
    }
    if f.is_show {
        // "N new episodes" is for a show IN PROGRESS: something watched and something left. A show
        // nobody opened has every episode unwatched, and "62 new episodes" tells a reader nothing.
        let in_progress = f.viewed_leaf_count.filter(|v| *v > 0);
        let unwatched = f.leaf_count.zip(in_progress).map(|(l, v)| l.saturating_sub(v)).filter(|n| *n > 0);
        return unwatched.map(|n| msg::captions_new_episodes_in(cx, i64::from(n)));
    }
    let dur = f.duration_min.filter(|m| *m > 0)?;
    let off = f.view_offset_min.filter(|m| *m > 0 && *m < dur)?;
    Some(crate::fmt::time_left_minutes_in(cx, i64::from(dur - off)))
}

/// What a tile IS, in one short line, for a tile the picked details kind has nothing to say about,
/// and **every kind's chain ends in a word, so a line the setting turns on is never drawn empty**:
///
/// - a collection its size, else "Collection";
/// - an episode its address ("S1 · E8", or its show when its art prints the address), else its
///   year, else "Episode";
/// - a season its own title ("Season 6", "Specials"), else "Season N" from the catalog, else
///   "Specials" for the season numbered 0 the server gave no title;
/// - a show its year, else "Show"; a movie its year, else "Movie".
///
/// `None` only for a tile that is none of those (a bare default with no kind flag).
///
/// **Why the last rung is the kind word and not the tile's name.** Under Title Off the word says
/// little the poster does not already show, and the tile's own name would say more. But the name is
/// the very line the reader switched off, and a name here would make the Title switch lie about its
/// own state; a kind word is true of every tile, needs no fact the server may not have sent, and
/// keeps the rule that a band the setting opens is never open over nothing. It is the last rung, so
/// it is read only by a tile with no year, address, title or size at all (an owner call, kept).
fn identity_line(f: &CaptionFacts, cx: &LocaleContext) -> Option<String> {
    use plx_platform::i18n::msg;
    if let Some(sub) = f.subtype.as_deref() {
        return Some(match f.child_count {
            Some(n) => crate::fmt::collection_count_in(cx, Some(sub), i64::from(n)),
            None => msg::browse_collection_kind_in(cx).to_string(),
        });
    }
    if let Some((number, own)) = &f.season {
        if f.address_on_art {
            // The art prints "SEASON 3": the line names the show, or nothing (never the number twice).
            return f.show.clone().filter(|show| !show.is_empty());
        }
        return Some(crate::fmt::season_identity_in(cx, i64::from(*number), own));
    }
    if let Some(line) = episode_line(f, cx) {
        return Some(line);
    }
    f.year.filter(|y| *y > 0).map(|y| y.to_string()).or_else(|| {
        if f.episode.is_some() {
            Some(msg::browse_kind_episode_in(cx).to_string())
        } else if f.is_show {
            Some(msg::browse_kind_show_in(cx).to_string())
        } else {
            f.is_movie.then(|| msg::browse_kind_movie_in(cx).to_string())
        }
    })
}

/// The caption's text shadow (`0 1px 8px` black 35%): the glyph path has no blur, so it is set as
/// an offset copy of the line, as `collection_tile`'s fan name is.
pub const CAPTION_SHADOW: [f32; 4] = [0.0, 0.0, 0.0, 0.35];
/// The shadow's downward offset.
pub const CAPTION_SHADOW_DY: f32 = 1.0;

#[cfg(test)]
#[path = "caption_policy_tests.rs"]
mod more_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use DetailPart as P;

    fn parts(ps: &[DetailPart]) -> DetailParts {
        DetailParts::of(ps)
    }

    #[test]
    fn default_is_year_length_and_year_seasons_without_title_and_one_line_tall() {
        let p = CaptionPolicy::default();
        assert_eq!(p, CaptionPolicy::DEFAULT);
        assert!(!p.title);
        assert_eq!(p.details, parts(&[P::Year, P::Length]));
        assert_eq!(p.show_details, parts(&[P::Year, P::Seasons]));
        assert_eq!(band_h(&p), 80.0);
    }

    /// A single-kind surface sizes its band by that kind's line alone; a mixed shelf keeps the room
    /// for either.
    #[test]
    fn a_single_kind_surface_is_sized_by_its_own_line_and_a_mixed_one_by_either() {
        let movies_nothing = CaptionPolicy { title: false, details: DetailParts::NONE, show_details: DetailParts::SHOW_DEFAULT };
        assert_eq!(band_h(&movies_nothing), BAND_ONE_LINE, "a mixed shelf holds the room for the show line");
        assert_eq!(band_h(&movies_nothing.only(false)), BAND_NONE, "a Movies grid with Movie details Nothing opens nothing");
        assert_eq!(band_h(&movies_nothing.only(true)), BAND_ONE_LINE, "a Shows grid still has its line");
        let titled = CaptionPolicy { title: true, ..movies_nothing };
        assert_eq!(band_h(&titled), BAND_TWO_LINES);
        assert_eq!(band_h(&titled.only(false)), BAND_ONE_LINE, "the title alone is one line, not two");
        assert_eq!(band_h(&titled.only(true)), BAND_TWO_LINES);
        assert_eq!(titled.only(true), CaptionPolicy::same(true, DetailParts::SHOW_DEFAULT));
    }

    #[test]
    fn band_height_is_zero_for_nothing_80_for_one_line_112_for_two() {
        let none = CaptionPolicy::same(false, DetailParts::NONE);
        let title_only = CaptionPolicy::same(true, DetailParts::NONE);
        let both = CaptionPolicy::same(true, parts(&[P::Genre]));
        assert_eq!([band_h(&none), band_h(&title_only), band_h(&both)], [0.0, 80.0, 112.0]);
    }

    #[test]
    fn lines_follow_the_policy_and_fall_back_to_identity_for_missing_facts() {
        let facts = CaptionFacts { title: Some("Heat".into()), year: Some(1995), genre: Some("Crime".into()), ..Default::default() };
        let l = plx_platform::i18n::current();
        let p = CaptionPolicy::same(true, parts(&[P::Genre]));
        assert_eq!(caption_lines(&p, &facts, l), [Some("Heat".into()), Some("Crime".into())]);
        // no stream facts: the line is the item's identity (its year), never an open band over nothing
        let p = CaptionPolicy::same(false, parts(&[P::VideoQuality]));
        assert_eq!(caption_lines(&p, &facts, l), [None, Some("1995".into())]);
        let p = CaptionPolicy::same(false, parts(&[P::Year, P::Length]));
        assert_eq!(caption_lines(&p, &facts, l), [None, Some("1995".into())]);
        let p = CaptionPolicy::same(true, DetailParts::NONE);
        assert_eq!(caption_lines(&p, &facts, l), [Some("Heat".into()), None], "Nothing has no fallback");
        assert_eq!(caption_lines(&CaptionPolicy::same(false, parts(&[P::Genre])), &CaptionFacts::default(), l), [None, None],
            "an item with no name and no facts has nothing to say");
    }

    /// **The line is the ticked parts' values joined with the middle dot in the fixed order**, whatever
    /// order they were ticked in; a part the tile has no fact for is skipped, and one that is the only
    /// tick falls to the identity line.
    #[test]
    fn the_line_joins_the_ticked_parts_in_the_fixed_order() {
        let l = plx_platform::i18n::english();
        let movie = CaptionFacts {
            year: Some(2024), duration_min: Some(112), genre: Some("Drama".into()), video_resolution: Some("4k".into()), hdr: true,
            content_rating: Some("PG-13".into()), audience_rating: Some(8.1), is_movie: true, ..Default::default()
        };
        let at = |ps: &[DetailPart]| caption_lines(&CaptionPolicy::same(false, parts(ps)), &movie, l)[1].clone();
        assert_eq!(at(&[P::Score, P::Year]).as_deref(), Some("2024 \u{b7} 8.1"));
        assert_eq!(at(&[P::Year, P::Length, P::Genre, P::VideoQuality]).as_deref(), Some("2024 \u{b7} 1h 52m \u{b7} Drama \u{b7} 4K \u{b7} HDR"));
        assert_eq!(at(&[P::AgeRating, P::Score]).as_deref(), Some("PG-13 \u{b7} 8.1"));
        assert_eq!(at(&[P::Seasons]).as_deref(), Some("2024"), "a movie has no seasons: its identity says the year");
        let show = CaptionFacts { is_show: true, year: Some(2019), season_count: Some(6), genre: Some("Mystery".into()), content_rating: Some("TV-14".into()), audience_rating: Some(8.4), ..Default::default() };
        let at = |ps: &[DetailPart]| caption_lines(&CaptionPolicy::same(false, parts(ps)), &show, l)[1].clone();
        assert_eq!(at(&[P::Year, P::Seasons]).as_deref(), Some("2019 \u{b7} 6 seasons"));
        assert_eq!(at(&[P::Seasons, P::Genre, P::AgeRating, P::Score]).as_deref(), Some("6 seasons \u{b7} Mystery \u{b7} TV-14 \u{b7} 8.4"));
    }

    /// The stand-ins that matter more than the year, length and seasons keep their rules, and only a
    /// set that ticks one of those three parts has them.
    #[test]
    fn a_started_movie_and_a_show_in_progress_say_their_status_only_for_the_year_length_seasons_parts() {
        let l = plx_platform::i18n::english();
        let started = CaptionFacts { year: Some(2024), duration_min: Some(112), view_offset_min: Some(74), genre: Some("Drama".into()), ..Default::default() };
        let at = |ps: &[DetailPart]| caption_lines(&CaptionPolicy::same(false, parts(ps)), &started, l)[1].clone();
        assert_eq!(at(&[P::Year, P::Length]).as_deref(), Some("38m left"));
        assert_eq!(at(&[P::Length]).as_deref(), Some("38m left"));
        assert_eq!(at(&[P::Genre]).as_deref(), Some("Drama"));
        let ep = CaptionFacts { title: Some("Pilot".into()), episode: Some((1, 8)), genre: Some("Drama".into()), ..Default::default() };
        assert_eq!(caption_lines(&CaptionPolicy::same(false, parts(&[P::Year])), &ep, l)[1].as_deref(), Some("S1 \u{b7} E8"));
        assert_eq!(caption_lines(&CaptionPolicy::same(false, parts(&[P::Score])), &ep, l)[1].as_deref(), Some("S1 \u{b7} E8"), "no fact: the identity");
    }

    #[test]
    fn a_collection_says_its_size_under_any_non_empty_set_and_nothing_under_the_empty_one() {
        let l = plx_platform::i18n::english();
        let films = CaptionFacts { subtype: Some("movie".into()), child_count: Some(7), ..Default::default() };
        for ps in [&[P::Genre][..], &[P::Year, P::Length], &[P::Score]] {
            assert_eq!(caption_lines(&CaptionPolicy::same(false, parts(ps)), &films, l)[1].as_deref(), Some("7 films"));
        }
        assert_eq!(caption_lines(&CaptionPolicy::same(false, DetailParts::NONE), &films, l)[1], None);
    }
}

// A movie or TV show library sorted by a value draws one value line under every poster instead. That mode is
// owned by Grid and TileLabel (`SortValue`); this two-line policy continues to describe every ordinary poster surface.
