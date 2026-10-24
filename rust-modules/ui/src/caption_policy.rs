//! **Under posters** — what a focused poster's caption band shows and how tall that band is.
//!
//! One global setting (Settings > Libraries > Under posters): a Title switch (default Off) and a
//! Details picker. Only the FOCUSED row opens its band: 80 px for one line, 112 px for two,
//! nothing when the policy shows nothing; non-focused rows keep their 40 px spacing. (The Continue Watching
//! deck is the one shelf handed a different policy: its progress line is always shown, so under Nothing
//! it is handed `registry::captions::deck_policy` and keeps its one line.)
//!
//! **A line the setting turns on is never drawn empty**, whatever the tile is. The title line is the
//! name a reader knows the tile by (a season by its SHOW, an episode by its own title). The details
//! line is the picked kind's facts, and a tile that has none of them (a season has no length and
//! often no genre or rating) says its identity line instead (a season "Season 6" or "Specials", an
//! episode its "S1 · E8" address, a show or movie its year, a collection its size), and a tile with
//! not even that says what KIND it is ("Episode", "Movie", "Show", "Collection"), so a band the
//! policy opens is never open over nothing. Under Nothing with Title Off no band opens at all (but for the deck).
//!
//! **One collection rule, on every surface.** A collection's details line is its size ("7 films")
//! under every details kind but Nothing; the Title switch governs its title line like any poster's;
//! and a name its face cuts ([`CaptionFacts::name_cut`]) is told on the details line when the title
//! line is off, since the face is the only other place it is told.
//!
//! **Every POSTER tile reads this one setting**: the Library grid, the Collections page, the
//! Watchlist, Home's shelves, the Library's Recommended shelves, Search's poster rows, the person
//! page's filmography, Detail's Related and collection shelves and the Collection screen's members
//! (the screens build the focused block through `registry::captions`, and hand a
//! [`crate::cards::Shelf`] / grid the policy so the room it reserves is [`band_h`] of the same value).
//! **Landscape tiles and headshots are not posters and are not on it**: an episode still (the
//! Library's Episodes listing, Search's Episode row, Recommended's landscape rows, Detail's Extras)
//! and a cast or search-result headshot keep their own block, in their own height (the legacy band,
//! `Grid::set_caption_policy(None)` for a grid), under every setting. The types and the two function signatures are what the
//! library, collection and settings packages compile against. [`band_h`] sizes the band;
//! [`caption_lines`] words the lines in the given locale, with the catalog's plurals.

use plx_platform::i18n::LocaleContext;

/// Band height with nothing to show.
pub const BAND_NONE: f32 = 0.0;
/// Band height for one caption line.
pub const BAND_ONE_LINE: f32 = 80.0;
/// Band height for two caption lines.
pub const BAND_TWO_LINES: f32 = 112.0;

/// Which facts the details line carries.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DetailsKind {
    /// "2019 · 2h 14m" (default).
    #[default]
    YearLength,
    Genre,
    /// "4K · HDR".
    Video,
    /// "PG-13 · 7.8".
    AgeRatingScore,
    None,
}

/// The setting.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CaptionPolicy {
    /// Show the title above the details.
    pub title: bool,
    pub details: DetailsKind,
}

impl CaptionPolicy {
    pub const DEFAULT: CaptionPolicy = CaptionPolicy { title: false, details: DetailsKind::YearLength };

    /// How many lines this policy can show at most (a tile with no name, or no fact and no identity,
    /// may leave fewer).
    pub fn max_lines(&self) -> usize {
        usize::from(self.title) + usize::from(self.details != DetailsKind::None)
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
/// The details line is the picked kind's facts, and a tile that has none of them (a season has no
/// length and often no genre or rating, a movie may have no genre) says its identity line instead
/// ([`identity_line`]; a movie, show or collection with not even that says what KIND it is, the last
/// fallback), so a band the policy opens is never open over nothing, and the source credit
/// ([`CaptionFacts::source`]) follows whatever the details line says. Under
/// [`DetailsKind::None`] there is no details line, no fallback and no credit; a line is `None` only
/// when the setting turns it off (or the tile has no name or identity at all), never for want of one
/// picked fact.
///
/// Every word on a line comes from the catalog in `locale` (the shared `core.*` / `browse.*`
/// duration, count and plural keys plus `captions.new_episodes`); the only literals are the
/// technical marks `HDR`, `4K`, `1080p` and the middle dot.
pub fn caption_lines(policy: &CaptionPolicy, facts: &CaptionFacts, locale: &LocaleContext) -> [Option<String>; 2] {
    let title = policy.title.then(|| facts.title.clone()).flatten().filter(|t| !t.is_empty());
    if policy.details == DetailsKind::None {
        return [title, None];
    }
    // A COLLECTION has one fact worth a line, its size, under every details kind (it has no genre,
    // stream or age rating to pick): one rule on every surface that shows one. A name its face cuts
    // is the only other place the name is told, so with the title line off it rides the details line.
    if facts.subtype.is_some() {
        let size = identity_line(facts, locale);
        let name = facts.title.clone().filter(|name| facts.name_cut && title.is_none() && !name.is_empty());
        return [title, join(name.into_iter().chain(size).chain(facts.source.clone().filter(|s| !s.is_empty())).collect())];
    }
    let picked = match policy.details {
        DetailsKind::None => unreachable!("returned above"),
        DetailsKind::YearLength => year_length(facts, locale),
        DetailsKind::Genre => facts.genre.clone().filter(|g| !g.is_empty()),
        DetailsKind::Video => join(
            facts.video_resolution.as_deref().and_then(resolution_mark).into_iter()
                .chain(facts.hdr.then(|| "HDR".to_string())).collect(),
        ),
        DetailsKind::AgeRatingScore => join(
            facts.content_rating.clone().into_iter().filter(|r| !r.is_empty())
                .chain(facts.audience_rating.filter(|s| *s > 0.0)
                    .map(|s| locale.decimal((s * 10.0).round() as i64, 1))).collect(),
        ),
    };
    let details = picked.or_else(|| identity_line(facts, locale));
    // The same words are never drawn twice: a season named by its show over its show, or an orphan
    // season over its own title, shows the one line.
    let details = details.filter(|line| title.as_ref() != Some(line));
    [title, join(details.into_iter().chain(facts.source.clone().filter(|s| !s.is_empty())).collect())]
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

/// The default details line, by kind: a collection is its size, an episode its address, a season its
/// own identity ("Season 6": it beats its year, which names neither the show nor the season), a show
/// its season count (or, once it is in progress, its unwatched new episodes), a started item the time
/// it has left, anything else year and length ([`identity_line`] answers when there is none).
fn year_length(f: &CaptionFacts, cx: &LocaleContext) -> Option<String> {
    use plx_platform::i18n::msg;
    if f.season.is_some() {
        return identity_line(f, cx);
    }
    if let Some(sub) = f.subtype.as_deref() {
        return Some(crate::fmt::collection_count_in(cx, Some(sub), i64::from(f.child_count?)));
    }
    if let Some(line) = episode_line(f, cx) {
        return Some(line);
    }
    let year = f.year.filter(|y| *y > 0).map(|y| y.to_string());
    if f.is_show {
        // "N new episodes" is for a show IN PROGRESS: something watched and something left. A show
        // nobody opened has every episode unwatched, and "62 new episodes" tells a reader nothing.
        let in_progress = f.viewed_leaf_count.filter(|v| *v > 0);
        let unwatched = f.leaf_count.zip(in_progress).map(|(l, v)| l.saturating_sub(v)).filter(|n| *n > 0);
        if let Some(n) = unwatched {
            return Some(msg::captions_new_episodes_in(cx, i64::from(n)));
        }
        let seasons = f.season_count.or(f.child_count).filter(|n| *n > 0)
            .map(|n| msg::browse_detail_seasons_in(cx, i64::from(n)));
        return join(year.into_iter().chain(seasons).collect());
    }
    let dur = f.duration_min.filter(|m| *m > 0);
    if let (Some(d), Some(off)) = (dur, f.view_offset_min.filter(|m| *m > 0)) {
        if off < d {
            return Some(crate::fmt::time_left_minutes_in(cx, i64::from(d - off)));
        }
    }
    let length = dur.map(|d| crate::fmt::duration_minutes_in(cx, i64::from(d)));
    join(year.into_iter().chain(length).collect())
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

    #[test]
    fn default_is_year_length_without_title_and_one_line_tall() {
        let p = CaptionPolicy::default();
        assert_eq!(p, CaptionPolicy::DEFAULT);
        assert!(!p.title);
        assert_eq!(p.details, DetailsKind::YearLength);
        assert_eq!(band_h(&p), 80.0);
    }

    #[test]
    fn band_height_is_zero_for_nothing_80_for_one_line_112_for_two() {
        let none = CaptionPolicy { title: false, details: DetailsKind::None };
        let title_only = CaptionPolicy { title: true, details: DetailsKind::None };
        let both = CaptionPolicy { title: true, details: DetailsKind::Genre };
        assert_eq!([band_h(&none), band_h(&title_only), band_h(&both)], [0.0, 80.0, 112.0]);
    }

    #[test]
    fn lines_follow_the_policy_and_fall_back_to_identity_for_missing_facts() {
        let facts = CaptionFacts { title: Some("Heat".into()), year: Some(1995), genre: Some("Crime".into()), ..Default::default() };
        let l = plx_platform::i18n::current();
        let p = CaptionPolicy { title: true, details: DetailsKind::Genre };
        assert_eq!(caption_lines(&p, &facts, l), [Some("Heat".into()), Some("Crime".into())]);
        // no stream facts: the line is the item's identity (its year), never an open band over nothing
        let p = CaptionPolicy { title: false, details: DetailsKind::Video };
        assert_eq!(caption_lines(&p, &facts, l), [None, Some("1995".into())]);
        let p = CaptionPolicy { title: false, details: DetailsKind::YearLength };
        assert_eq!(caption_lines(&p, &facts, l), [None, Some("1995".into())]);
        let p = CaptionPolicy { title: true, details: DetailsKind::None };
        assert_eq!(caption_lines(&p, &facts, l), [Some("Heat".into()), None], "Nothing has no fallback");
        assert_eq!(caption_lines(&CaptionPolicy { title: false, details: DetailsKind::Genre }, &CaptionFacts::default(), l), [None, None],
            "an item with no name and no facts has nothing to say");
    }
}
